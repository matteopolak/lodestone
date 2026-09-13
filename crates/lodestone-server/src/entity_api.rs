//! Typed server-side entity observation and mutation over the authoritative stores.
//!
//! [`ServerEntityApi`] is the plugin-facing boundary for a host that needs one
//! copied observation or one bounded mutation without borrowing a simulation
//! guard. It composes the live [`crate::MobHandle`] and connected
//! [`crate::PlayerRegistry`], and it also implements [`crate::EntitySource`]
//! so the same handles can be passed directly to a serving constructor.
//!
//! The API uses [`lodestone_model::EntityNetworkId`] for every plugin-facing
//! entity reference. Raw integers remain at the mob/player stores and at the
//! [`crate::EntitySource`] protocol boundary; callers crossing in from those
//! surfaces must classify a wire id explicitly with
//! [`lodestone_model::EntityNetworkId::from_wire`].

use std::collections::HashMap;

use lodestone_model::{EntityEquipment, EntityNetworkId, ResourceKey, Rotation, Vec3};
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
    pub id: EntityNetworkId,
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
    /// The six equipment slots, including empty slots. Player values are read
    /// from the connection-owned inventory mirror; mob values are copied from
    /// the authoritative species-spawn state.
    pub equipment: Vec<EntityEquipment>,
}

/// A lifecycle edge observed by a plugin-owned [`EntityLifecycleCursor`].
#[derive(Debug, Clone, PartialEq)]
pub enum EntityLifecycleEvent {
    /// The entity was present when the cursor first observed it.
    Spawned(EntityObservation),
    /// The entity was present in the previous poll and is no longer live.
    Despawned(EntityObservation),
}

/// Caller-owned cursor for bounded entity lifecycle observation.
///
/// The cursor stores only copied observations keyed by the server entity id.
/// It never retains an ECS entity, a mob/player lock guard, or a callback into
/// the connection task. A fresh cursor reports every currently live entity as
/// `Spawned`; subsequent polls report only additions and removals. Changes to
/// an existing entity are available through [`ServerEntityApi::observe`] and
/// deliberately do not create an unbounded event log here.
#[derive(Debug, Default)]
pub struct EntityLifecycleCursor {
    known: HashMap<EntityNetworkId, EntityObservation>,
}

impl EntityLifecycleCursor {
    /// Creates a cursor that starts at the next poll's live population.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns lifecycle edges since the previous poll and advances the
    /// cursor. Events are sorted by entity id for deterministic plugin code and
    /// tests, independent of the mob/player store iteration order.
    pub fn poll(&mut self, api: &ServerEntityApi) -> Vec<EntityLifecycleEvent> {
        let current: HashMap<_, _> = api
            .observations()
            .into_iter()
            .map(|observation| (observation.id, observation))
            .collect();
        let mut events = Vec::new();

        for (id, observation) in &current {
            if !self.known.contains_key(id) {
                events.push(EntityLifecycleEvent::Spawned(observation.clone()));
            }
        }
        for (id, observation) in &self.known {
            if !current.contains_key(id) {
                events.push(EntityLifecycleEvent::Despawned(observation.clone()));
            }
        }
        events.sort_by_key(|event| match event {
            EntityLifecycleEvent::Spawned(observation)
            | EntityLifecycleEvent::Despawned(observation) => observation.id,
        });
        self.known = current;
        events
    }
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
    pub fn observe(&self, id: EntityNetworkId) -> Option<EntityObservation> {
        let raw_id = server_entity_raw(id)?;
        if let Some(observation) = self.mobs.with(|sim| {
            sim.get(raw_id).map(|mob| EntityObservation {
                id,
                uuid: mob.uuid(),
                entity_type: mob.entity_type().clone(),
                position: mob.position(),
                rotation: mob.rotation(),
                velocity: mob.velocity(),
                health: Some(mob.health()),
                max_health: Some(mob.max_health()),
                equipment: mob.equipment_snapshot(),
            })
        }) {
            return Some(observation);
        }

        self.players
            .candidates()
            .into_iter()
            .find(|player| player.entity_id == raw_id)
            .map(|player| EntityObservation {
                id: server_entity_id(raw_id),
                uuid: player.uuid,
                entity_type: "minecraft:player"
                    .parse()
                    .expect("the built-in player entity key is valid"),
                position: player.position,
                rotation: player.rotation,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                health: None,
                max_health: None,
                equipment: self
                    .players
                    .inventory(player.uuid)
                    .map_or_else(Vec::new, |inventory| inventory.equipment_snapshot()),
            })
    }

    /// Copies every currently live mob and connected player into owned
    /// observations. This is the snapshot producer used by
    /// [`EntityLifecycleCursor`]; no internal guard escapes the call.
    #[must_use]
    pub fn observations(&self) -> Vec<EntityObservation> {
        let mut observations = self
            .mobs
            .with(|sim| {
                sim.iter()
                    .map(|mob| EntityObservation {
                        id: server_entity_id(mob.id()),
                        uuid: mob.uuid(),
                        entity_type: mob.entity_type().clone(),
                        position: mob.position(),
                        rotation: mob.rotation(),
                        velocity: mob.velocity(),
                        health: Some(mob.health()),
                        max_health: Some(mob.max_health()),
                        equipment: mob.equipment_snapshot(),
                    })
                    .collect::<Vec<_>>()
            });
        observations.extend(self.players.candidates().into_iter().map(|player| {
            EntityObservation {
                id: server_entity_id(player.entity_id),
                uuid: player.uuid,
                entity_type: "minecraft:player"
                    .parse()
                    .expect("the built-in player entity key is valid"),
                position: player.position,
                rotation: player.rotation,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                health: None,
                max_health: None,
                equipment: self
                    .players
                    .inventory(player.uuid)
                    .map_or_else(Vec::new, |inventory| inventory.equipment_snapshot()),
            }
        }));
        observations.sort_by_key(|observation| observation.id);
        observations
    }

    /// Apply one typed operation against the authoritative mob or player store.
    /// Player-facing operations are queued rather than run under a foreign
    /// connection borrow; the owning connection then emits the protocol frame.
    pub fn mutate(&self, id: EntityNetworkId, mutation: EntityMutation) -> EntityMutationResult {
        let Some(id) = server_entity_raw(id) else {
            return EntityMutationResult::UnknownEntity;
        };
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
    pub fn spawn(&self, entity_type: ResourceKey, position: Vec3) -> EntityNetworkId {
        self.mobs.with(|sim| server_entity_id(sim.spawn_species(entity_type, position).id()))
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

/// Converts a server-owned id at the store boundary without admitting a local
/// plugin id or a value that cannot be represented by the server's `i32` maps.
fn server_entity_raw(id: EntityNetworkId) -> Option<i32> {
    match id {
        EntityNetworkId::Server(raw) => i32::try_from(raw).ok(),
        EntityNetworkId::Plugin(_) => None,
    }
}

/// Classifies an id produced by the authoritative server stores.
fn server_entity_id(raw: i32) -> EntityNetworkId {
    EntityNetworkId::from_wire(raw).expect("server entity ids must be non-negative")
}

impl EntitySource for ServerEntityApi {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.mobs.snapshots()
    }

    fn players(&self) -> Option<&PlayerRegistry> {
        Some(&self.players)
    }
}
