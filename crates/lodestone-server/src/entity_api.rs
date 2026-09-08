//! Typed server-side entity observation and mutation over the authoritative stores.
//!
//! [`ServerEntityApi`] is the plugin-facing boundary for a host that needs one
//! copied observation or one bounded mutation without borrowing a simulation
//! guard. It composes the live [`crate::MobHandle`] and connected
//! [`crate::PlayerRegistry`], and it also implements [`crate::EntitySource`]
//! so the same handles can be passed directly to a serving constructor.
//!
//! The API intentionally keeps the existing network id representation at its
//! boundary. A future canonical id wrapper can replace that field without
//! changing the operation or observation shapes.

use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::commands::Effect;
use crate::mobs::MobHandle;
use crate::players::PlayerRegistry;
use crate::protocol::EntitySnapshot;
use crate::server::EntitySource;

/// A copied view of one live entity.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityObservation {
    /// The entity's network id within this server session.
    pub id: i32,
    /// The entity's stable identity.
    pub uuid: Uuid,
    /// The canonical entity type.
    pub entity_type: ResourceKey,
    /// World-space feet position.
    pub position: Vec3,
    /// Body rotation in degrees.
    pub rotation: Rotation,
    /// Current motion in blocks per tick, where available.
    pub velocity: Vec3,
    /// Current health for a mob. Player health remains connection-owned and is
    /// therefore unavailable through the shared player registry.
    pub health: Option<f32>,
    /// Maximum health for a mob.
    pub max_health: Option<f32>,
}

/// A server-authoritative operation against one entity.
#[derive(Debug, Clone, PartialEq)]
pub enum EntityMutation {
    /// Apply one motion impulse to a mob. The next snapshot carries the new
    /// velocity through the normal entity update encoder.
    ApplyKnockback(Vec3),
    /// Set a mob's health, clamped by the mob model's own setter.
    SetHealth(f32),
    /// Apply one status effect. Player effects are queued for the owning
    /// connection so the effect packet and gameplay state remain together.
    ApplyEffect {
        effect: String,
        duration: i32,
        amplifier: u32,
    },
    /// Ask a connected player to relocate through the directed effect queue.
    /// The player connection owns teleport acknowledgement and packet output.
    Teleport {
        position: Vec3,
        rotation: Option<Rotation>,
    },
    /// Remove one mob without death drops or experience.
    Despawn,
}

/// The outcome of an entity mutation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityMutationResult {
    /// The operation was accepted by the authoritative store.
    Applied,
    /// The id was not present in either authoritative store.
    UnknownEntity,
    /// The entity exists, but this operation is not supported for its kind.
    Unsupported,
}

/// A cloneable, lock-bounded entity capability over one live server world.
#[derive(Debug, Clone)]
pub struct ServerEntityApi {
    mobs: MobHandle,
    players: PlayerRegistry,
}

impl ServerEntityApi {
    /// Compose the exact mob and player handles owned by one server world.
    #[must_use]
    pub fn new(mobs: MobHandle, players: PlayerRegistry) -> Self {
        Self { mobs, players }
    }

    /// Copy the requested entity's current identity, motion and health fields.
    /// No lock guard or internal entity reference escapes this call.
    #[must_use]
    pub fn observe(&self, id: i32) -> Option<EntityObservation> {
        if let Some(observation) = self.mobs.with(|sim| {
            sim.get(id).map(|mob| EntityObservation {
                id,
                uuid: mob.uuid(),
                entity_type: mob.entity_type().clone(),
                position: mob.position(),
                rotation: mob.rotation(),
                velocity: mob.velocity(),
                health: Some(mob.health()),
                max_health: Some(mob.max_health()),
            })
        }) {
            return Some(observation);
        }

        self.players
            .candidates()
            .into_iter()
            .find(|player| player.entity_id == id)
            .map(|player| EntityObservation {
                id,
                uuid: player.uuid,
                entity_type: "minecraft:player"
                    .parse()
                    .expect("the built-in player entity key is valid"),
                position: player.position,
                rotation: player.rotation,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                health: None,
                max_health: None,
            })
    }

    /// Apply one typed operation against the authoritative mob or player store.
    /// Player-facing operations are queued rather than run under a foreign
    /// connection borrow; the owning connection then emits the protocol frame.
    pub fn mutate(&self, id: i32, mutation: EntityMutation) -> EntityMutationResult {
        let player_exists = self.player_exists(id);
        match mutation {
            EntityMutation::ApplyKnockback(impulse) => self.mobs.with(|sim| {
                let Some(mob) = sim.get_mut(id) else {
                    return if player_exists {
                        EntityMutationResult::Unsupported
                    } else {
                        EntityMutationResult::UnknownEntity
                    };
                };
                mob.apply_knockback(impulse);
                EntityMutationResult::Applied
            }),
            EntityMutation::SetHealth(health) => self.mobs.with(|sim| {
                let Some(mob) = sim.get_mut(id) else {
                    return if player_exists {
                        EntityMutationResult::Unsupported
                    } else {
                        EntityMutationResult::UnknownEntity
                    };
                };
                mob.set_health(health);
                EntityMutationResult::Applied
            }),
            EntityMutation::ApplyEffect {
                effect,
                duration,
                amplifier,
            } => {
                if self
                    .mobs
                    .with(|sim| sim.get_mut(id).map(|mob| mob.apply_effect(&effect, duration, amplifier)))
                    .is_some()
                {
                    return EntityMutationResult::Applied;
                }
                let Some(player) = self.player(id) else {
                    return EntityMutationResult::UnknownEntity;
                };
                self.players
                    .push_effect(
                        player.uuid,
                        Effect::ApplyEffect {
                            effect,
                            duration,
                            amplifier,
                        },
                    )
                    .then_some(EntityMutationResult::Applied)
                    .unwrap_or(EntityMutationResult::UnknownEntity)
            }
            EntityMutation::Teleport { position, rotation } => {
                let Some(player) = self.player(id) else {
                    return if self.mob_exists(id) {
                        EntityMutationResult::Unsupported
                    } else {
                        EntityMutationResult::UnknownEntity
                    };
                };
                self.players
                    .push_effect(
                        player.uuid,
                        Effect::Teleport {
                            x: position.x,
                            y: position.y,
                            z: position.z,
                            yaw: rotation.map(|value| value.yaw),
                            pitch: rotation.map(|value| value.pitch),
                        },
                    )
                    .then_some(EntityMutationResult::Applied)
                    .unwrap_or(EntityMutationResult::UnknownEntity)
            }
            EntityMutation::Despawn => self.mobs.with(|sim| {
                if sim.remove_mob(id) {
                    EntityMutationResult::Applied
                } else if player_exists {
                    EntityMutationResult::Unsupported
                } else {
                    EntityMutationResult::UnknownEntity
                }
            }),
        }
    }

    /// Spawn a mob into the same simulation the entity source streams.
    #[must_use]
    pub fn spawn(&self, entity_type: ResourceKey, position: Vec3) -> i32 {
        self.mobs
            .with(|sim| sim.spawn_species(entity_type, position).id())
    }

    fn player(&self, id: i32) -> Option<crate::commands::PlayerCandidate> {
        self.players
            .candidates()
            .into_iter()
            .find(|player| player.entity_id == id)
    }

    fn player_exists(&self, id: i32) -> bool {
        self.player(id).is_some()
    }

    fn mob_exists(&self, id: i32) -> bool {
        self.mobs.with(|sim| sim.get(id).is_some())
    }
}

impl EntitySource for ServerEntityApi {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.mobs.snapshots()
    }

    fn players(&self) -> Option<&PlayerRegistry> {
        Some(&self.players)
    }
}
