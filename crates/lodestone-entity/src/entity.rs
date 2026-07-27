//! Version-free entity tracking.
//!
//! [`EntityTracker`] is a store keyed by the server's entity id. It holds the
//! version-free per-entity state — position, velocity, rotation, type, UUID and
//! a metadata bag — and exposes the packet-driven lifecycle the network layer
//! feeds it: spawn, relative and absolute moves, rotation, velocity, teleport
//! and removal.
//!
//! Nothing here knows a protocol version. A version crate lifts wire packets
//! into these method calls (canonical [`Vec3`] deltas, canonical type keys), so
//! the tracker is identical whether it is fed by a 1.8 or a 26.2 adapter.
//!
//! Position updates arrive at ~20 Hz. Each move stores the previous position
//! into an [`Interpolated`] so a renderer can blend between confirmations
//! instead of snapping; see [`crate::interpolation`].

use crate::interpolation::Interpolated;
use crate::metadata::EntityMetadata;
use lodestone_model::{Identifier, Rotation, Vec3};
use std::collections::HashMap;
use uuid::Uuid;

/// The canonical type of an entity: a namespaced key plus the optional numeric
/// registry id the spawning version used (kept for round-tripping, never
/// interpreted here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKind {
    /// Namespaced type key, e.g. `minecraft:pig`.
    pub key: Identifier,
    /// The numeric registry id from the spawning version, if known.
    pub numeric_id: Option<i32>,
}

impl EntityKind {
    /// Creates a kind from a key with no numeric id.
    #[must_use]
    pub fn new(key: Identifier) -> Self {
        Self {
            key,
            numeric_id: None,
        }
    }

    /// Creates a kind from a key and a numeric registry id.
    #[must_use]
    pub fn with_id(key: Identifier, numeric_id: i32) -> Self {
        Self {
            key,
            numeric_id: Some(numeric_id),
        }
    }
}

/// The tracked state of one entity.
///
/// `position` and `rotation` are the authoritative server-confirmed values.
/// Their [`Interpolated`] wrappers additionally remember the previous
/// confirmation so a renderer can interpolate; call [`EntityState::render_pos`]
/// / [`EntityState::render_rotation`] with a render alpha.
#[derive(Debug, Clone)]
pub struct EntityState {
    /// Server entity id.
    pub id: i32,
    /// Entity UUID, when the spawn provided one.
    pub uuid: Option<Uuid>,
    /// Canonical entity type.
    pub kind: EntityKind,
    /// Authoritative position with interpolation history.
    pub position: Interpolated<Vec3>,
    /// Current velocity (blocks/tick in canonical units).
    pub velocity: Vec3,
    /// Body rotation with interpolation history.
    pub rotation: Interpolated<Rotation>,
    /// Head yaw (living entities), with interpolation history.
    pub head_yaw: Interpolated<f32>,
    /// Whether the server last reported the entity on the ground.
    pub on_ground: bool,
    /// Version-free metadata bag.
    pub metadata: EntityMetadata,
}

impl EntityState {
    /// The interpolated render position at alpha `t` in `[0, 1]`.
    #[must_use]
    pub fn render_pos(&self, t: f64) -> Vec3 {
        self.position.sample(t)
    }

    /// The interpolated render rotation at alpha `t` in `[0, 1]`.
    #[must_use]
    pub fn render_rotation(&self, t: f32) -> Rotation {
        self.rotation.sample(t)
    }

    /// The authoritative (latest confirmed) position.
    #[must_use]
    pub fn pos(&self) -> Vec3 {
        self.position.current
    }
}

/// A store of tracked entities keyed by server id.
#[derive(Debug, Clone, Default)]
pub struct EntityTracker {
    entities: HashMap<i32, EntityState>,
}

impl EntityTracker {
    /// Creates an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns (or replaces) an entity.
    ///
    /// Returns the previous state if an entity with the same id was present —
    /// which the network layer treats as a protocol error, but the tracker
    /// stays permissive.
    pub fn spawn(
        &mut self,
        id: i32,
        uuid: Option<Uuid>,
        kind: EntityKind,
        position: Vec3,
        rotation: Rotation,
        velocity: Vec3,
    ) -> Option<EntityState> {
        let state = EntityState {
            id,
            uuid,
            kind,
            position: Interpolated::new(position),
            velocity,
            rotation: Interpolated::new(rotation),
            head_yaw: Interpolated::new(rotation.yaw),
            on_ground: false,
            metadata: EntityMetadata::new(),
        };
        self.entities.insert(id, state)
    }

    /// Applies a relative move (a delta added to the current position). Vanilla
    /// sends these as quantised shorts; the version crate has already turned
    /// them into a canonical delta. Interpolation history is advanced.
    pub fn move_relative(&mut self, id: i32, delta: Vec3, on_ground: bool) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            let target = e.position.current + delta;
            e.position.push(target);
            e.on_ground = on_ground;
            true
        } else {
            false
        }
    }

    /// Applies an absolute move (position set, interpolated from the previous).
    pub fn move_absolute(&mut self, id: i32, position: Vec3, on_ground: bool) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            e.position.push(position);
            e.on_ground = on_ground;
            true
        } else {
            false
        }
    }

    /// Updates only the rotation, interpolated from the previous.
    pub fn set_rotation(&mut self, id: i32, rotation: Rotation, on_ground: bool) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            e.rotation.push(rotation);
            e.on_ground = on_ground;
            true
        } else {
            false
        }
    }

    /// Applies a combined relative move and rotation (vanilla's move-and-look).
    pub fn move_relative_and_rotate(
        &mut self,
        id: i32,
        delta: Vec3,
        rotation: Rotation,
        on_ground: bool,
    ) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            let target = e.position.current + delta;
            e.position.push(target);
            e.rotation.push(rotation);
            e.on_ground = on_ground;
            true
        } else {
            false
        }
    }

    /// Updates the head yaw, interpolated from the previous.
    pub fn set_head_yaw(&mut self, id: i32, head_yaw: f32) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            e.head_yaw.push(head_yaw);
            true
        } else {
            false
        }
    }

    /// Sets the velocity vector.
    pub fn set_velocity(&mut self, id: i32, velocity: Vec3) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            e.velocity = velocity;
            true
        } else {
            false
        }
    }

    /// Hard-teleports an entity, collapsing interpolation so it does not slide
    /// across the world.
    pub fn teleport(
        &mut self,
        id: i32,
        position: Vec3,
        rotation: Rotation,
        on_ground: bool,
    ) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            e.position.snap(position);
            e.rotation.snap(rotation);
            e.on_ground = on_ground;
            true
        } else {
            false
        }
    }

    /// Merges a metadata update into an entity's bag.
    pub fn apply_metadata(&mut self, id: i32, update: EntityMetadata) -> bool {
        if let Some(e) = self.entities.get_mut(&id) {
            for (index, value) in update.iter() {
                e.metadata.set(index, value.clone());
            }
            true
        } else {
            false
        }
    }

    /// Removes an entity, returning its final state if present.
    pub fn remove(&mut self, id: i32) -> Option<EntityState> {
        self.entities.remove(&id)
    }

    /// Removes several entities (vanilla's batched remove packet).
    pub fn remove_many(&mut self, ids: &[i32]) {
        for id in ids {
            self.entities.remove(id);
        }
    }

    /// The state for `id`, if tracked.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&EntityState> {
        self.entities.get(&id)
    }

    /// A mutable reference to the state for `id`, if tracked.
    pub fn get_mut(&mut self, id: i32) -> Option<&mut EntityState> {
        self.entities.get_mut(&id)
    }

    /// Whether `id` is tracked.
    #[must_use]
    pub fn contains(&self, id: i32) -> bool {
        self.entities.contains_key(&id)
    }

    /// The number of tracked entities.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Iterates all tracked entities.
    pub fn iter(&self) -> impl Iterator<Item = &EntityState> {
        self.entities.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn pig() -> EntityKind {
        EntityKind::with_id(Identifier::from_str("minecraft:pig").unwrap(), 82)
    }

    fn spawn_pig(t: &mut EntityTracker) {
        t.spawn(
            1,
            Some(Uuid::from_u128(1)),
            pig(),
            Vec3::new(0.0, 64.0, 0.0),
            Rotation::new(0.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
        );
    }

    #[test]
    fn spawn_and_get() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        assert_eq!(t.len(), 1);
        assert_eq!(t.get(1).unwrap().kind.key.path(), "pig");
        assert_eq!(t.get(1).unwrap().pos(), Vec3::new(0.0, 64.0, 0.0));
    }

    #[test]
    fn relative_move_accumulates() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        assert!(t.move_relative(1, Vec3::new(0.5, 0.0, 0.0), true));
        assert!(t.move_relative(1, Vec3::new(0.5, 0.0, 0.0), true));
        assert_eq!(t.get(1).unwrap().pos(), Vec3::new(1.0, 64.0, 0.0));
        // previous confirmation is 0.5, so half-render is 0.75.
        assert_eq!(t.get(1).unwrap().render_pos(0.5).x, 0.75);
    }

    #[test]
    fn absolute_move_sets_position() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        t.move_absolute(1, Vec3::new(10.0, 64.0, 10.0), true);
        assert_eq!(t.get(1).unwrap().pos(), Vec3::new(10.0, 64.0, 10.0));
    }

    #[test]
    fn teleport_snaps_no_interpolation() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        t.move_absolute(1, Vec3::new(1.0, 64.0, 0.0), true);
        t.teleport(
            1,
            Vec3::new(1000.0, 5.0, 1000.0),
            Rotation::new(90.0, 0.0),
            true,
        );
        let e = t.get(1).unwrap();
        assert_eq!(e.render_pos(0.0), Vec3::new(1000.0, 5.0, 1000.0));
        assert_eq!(e.render_pos(0.5), Vec3::new(1000.0, 5.0, 1000.0));
    }

    #[test]
    fn velocity_and_rotation() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        t.set_velocity(1, Vec3::new(0.1, 0.0, -0.2));
        t.set_rotation(1, Rotation::new(45.0, 10.0), true);
        let e = t.get(1).unwrap();
        assert_eq!(e.velocity, Vec3::new(0.1, 0.0, -0.2));
        assert_eq!(e.rotation.current, Rotation::new(45.0, 10.0));
    }

    #[test]
    fn remove_and_remove_many() {
        let mut t = EntityTracker::new();
        spawn_pig(&mut t);
        t.spawn(
            2,
            None,
            pig(),
            Vec3::new(0.0, 64.0, 0.0),
            Rotation::default(),
            Vec3::default(),
        );
        t.remove_many(&[1, 2, 3]);
        assert!(t.is_empty());
    }

    #[test]
    fn moves_to_unknown_entity_are_noops() {
        let mut t = EntityTracker::new();
        assert!(!t.move_relative(99, Vec3::default(), true));
        assert!(!t.set_velocity(99, Vec3::default()));
    }
}
