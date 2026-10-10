//! `MobSim`'s cushion slice: spawn, seat, break, and the periodic support check.
//!
//! # What it is
//!
//! A cushion is a block-attached entity, like a painting: stationary, no AI, no
//! health. It lives in its own map ([`super::TrackedCushion`]) and streams from
//! [`MobSim::push_cushion_snapshots`] with its colour as a metadata field. The
//! placement rules are [`crate::cushion`]'s; this file owns what happens to a
//! placed one.
//!
//! # How it works
//!
//! * One passenger at most. [`MobSim::mount_cushion`] refuses a sneaking
//!   player and an occupied cushion (the current rider included).
//! * A hit by a player breaks it ([`MobSim::break_cushion`]): the entity goes
//!   and its coloured item drops, unless the breaker is in creative.
//! * Every 101st tick an unseated cushion checks itself: fire in its box breaks
//!   it, and a box that no longer [survives](crate::cushion::would_survive_at)
//!   breaks it. [`MobSim::plan_cushion_checks`] only decides; the tick driver
//!   applies the result when the world read was complete.
//!
//! # How to change it
//!
//! Persistence is not wired: cushions are not saved with the world. Sounds and
//! break particles are not emitted.

use lodestone_data::block_states::StateId;
use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use super::{MobSim, TrackedCushion};
use crate::cushion::{self, Aabb};

/// Ticks a cushion waits between support checks; the check runs on the tick
/// after the counter has reached this.
const CHECK_INTERVAL: u32 = 100;

/// Upward speed of the dropped item, in blocks per tick.
const DROP_LIFT: f64 = 0.2;

pub(super) fn cushion_entity_type() -> ResourceKey {
    "minecraft:cushion".parse().expect("`minecraft:cushion` is a valid resource key")
}

/// What breaking a cushion left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CushionBroken {
    /// The player entity id that was seated on it, if any.
    pub rider: Option<i32>,
    /// The dye ordinal it had.
    pub color: u8,
}

impl<'w> MobSim<'w> {
    /// Creates a cushion standing at `position`, facing `yaw`, of dye ordinal
    /// `color`, and returns its network entity id.
    pub fn spawn_cushion(&mut self, position: Vec3, yaw: f32, color: u8) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.cushions.insert(
            id,
            TrackedCushion {
                uuid: Uuid::new_v4(),
                position,
                yaw,
                color: color & 0x0F,
                rider: None,
                ticks_since_check: 0,
            },
        );
        id
    }

    /// The number of live cushions.
    #[must_use]
    pub fn cushion_count(&self) -> usize {
        self.cushions.len()
    }

    /// A cushion's `(position, yaw, colour)`, if `id` is one.
    #[must_use]
    pub fn cushion_state(&self, id: i32) -> Option<(Vec3, f32, u8)> {
        self.cushions.get(&id).map(|c| (c.position, c.yaw, c.color))
    }

    /// Whether `id` is a cushion.
    #[must_use]
    pub fn is_cushion(&self, id: i32) -> bool {
        self.cushions.contains_key(&id)
    }

    /// Whether any live cushion's box overlaps `bb`.
    #[must_use]
    pub fn cushion_occupies(&self, bb: &Aabb) -> bool {
        self.cushions
            .values()
            .any(|c| Aabb::cushion_at(c.position).intersects(bb))
    }

    /// The player entity id seated on cushion `id`.
    #[must_use]
    pub fn cushion_rider(&self, id: i32) -> Option<i32> {
        self.cushions.get(&id).and_then(|c| c.rider)
    }

    /// The cushion `player_entity_id` is seated on.
    #[must_use]
    pub fn cushion_ridden_by(&self, player_entity_id: i32) -> Option<i32> {
        self.cushions
            .iter()
            .find(|(_, c)| c.rider == Some(player_entity_id))
            .map(|(&id, _)| id)
    }

    /// Seats a player on cushion `id`. Returns whether they are now aboard, the
    /// caller's cue to send the passenger list.
    ///
    /// Refused for a sneaking player and for an occupied cushion. A player
    /// seated elsewhere is lifted off that seat first.
    pub fn mount_cushion(&mut self, id: i32, player_entity_id: i32, using_secondary_action: bool) -> bool {
        if using_secondary_action || self.cushions.get(&id).is_none_or(|c| c.rider.is_some()) {
            return false;
        }
        self.dismount_rider(player_entity_id);
        self.dismount_minecart_rider(player_entity_id);
        self.dismount_mob(player_entity_id);
        if let Some(previous) = self.cushion_ridden_by(player_entity_id)
            && let Some(old) = self.cushions.get_mut(&previous)
        {
            old.rider = None;
        }
        if let Some(cushion) = self.cushions.get_mut(&id) {
            cushion.rider = Some(player_entity_id);
        }
        true
    }

    /// Lifts `player_entity_id` off its cushion, returning the cushion id.
    pub fn dismount_cushion_rider(&mut self, player_entity_id: i32) -> Option<i32> {
        let id = self.cushion_ridden_by(player_entity_id)?;
        if let Some(cushion) = self.cushions.get_mut(&id) {
            cushion.rider = None;
        }
        Some(id)
    }

    /// Removes cushion `id`; when `drop_item` is set its coloured item drops at
    /// its position. `None` when `id` is not a cushion.
    pub fn break_cushion(&mut self, id: i32, drop_item: bool) -> Option<CushionBroken> {
        let cushion = self.cushions.remove(&id)?;
        if drop_item {
            self.spawn_item(
                cushion::item_for_color(cushion.color),
                cushion.position,
                Vec3::new(0.0, DROP_LIFT, 0.0),
                lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                    1,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
        Some(CushionBroken {
            rider: cushion.rider,
            color: cushion.color,
        })
    }

    /// Advances every cushion's check counter one tick and returns the ids whose
    /// check fired and found fire in the box or no surviving support. Nothing is
    /// removed: the driver calls [`break_cushion`](Self::break_cushion) once it
    /// knows the world read behind `state` was complete.
    pub(crate) fn plan_cushion_checks(&mut self, state: &dyn Fn(i32, i32, i32) -> StateId) -> Vec<i32> {
        let mut ids: Vec<i32> = self.cushions.keys().copied().collect();
        ids.sort_unstable();
        ids.retain(|id| {
            let Some(c) = self.cushions.get_mut(id) else {
                return false;
            };
            if c.ticks_since_check < CHECK_INTERVAL {
                c.ticks_since_check += 1;
                return false;
            }
            c.ticks_since_check = 0;
            // A seated cushion is left alone: removing it from under a player
            // needs a passenger-list update to that player's connection, which
            // the tick has no handle to.
            if c.rider.is_some() {
                return false;
            }
            let bb = Aabb::cushion_at(c.position);
            cushion::fire_in(&bb, state) || !cushion::would_survive_at(&bb, state)
        });
        ids
    }

    pub(super) fn clear_disconnected_cushion_riders(&mut self, connected: &[i32]) {
        for cushion in self.cushions.values_mut() {
            if cushion.rider.is_some_and(|rider| !connected.contains(&rider)) {
                cushion.rider = None;
            }
        }
    }

    pub(super) fn push_cushion_snapshots(&self, out: &mut Vec<crate::protocol::EntitySnapshot>) {
        let mut ids: Vec<i32> = self.cushions.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(c) = self.cushions.get(&id) else {
                continue;
            };
            out.push(crate::protocol::EntitySnapshot {
                id,
                uuid: c.uuid,
                entity_type: cushion_entity_type(),
                position: c.position,
                rotation: Rotation::new(c.yaw, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                on_ground: false,
                metadata: vec![crate::protocol::MetadataField::CushionColor(c.color)],
                object_data: 0,
                equipment: Vec::new(),
                leash_link: None,
            });
        }
    }
}
