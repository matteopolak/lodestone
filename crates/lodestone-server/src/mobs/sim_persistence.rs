//! Persistence and despawn-facing entity lifecycle operations.

use super::*;

impl<'w> MobSim<'w> {
    /// Every mob and dropped item in this sim, as the records
    /// [`crate::entity_storage`] persists.
    ///
    /// # Why this is not [`snapshots`](Self::snapshots)
    ///
    /// [`EntitySnapshot`] is the *wire* view: it carries an `id` (a per-session
    /// entity id that means nothing across a restart), no health, and no item
    /// lifecycle. A save built from it would come back as full-health mobs and
    /// dropped items that never despawn. This is the disk view, and the two
    /// deliberately do not share a type.
    ///
    /// **Projectiles are excluded.** An arrow in flight has no persisted
    /// identity in this sim (`ProjectileMeta` carries a uuid and a type but the
    /// registry holds no owner, no pickup state and no damage), so writing one
    /// would persist an object we could not faithfully restore. Vanilla does
    /// save them; that is a follow-up, and it is named in `docs/entity-persistence.md`
    /// rather than left to be discovered as a missing mob.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn saved_entities(&self) -> Vec<crate::entity_storage::SavedEntity> {
        let mut out: Vec<crate::entity_storage::SavedEntity> = self
            .mobs
            .iter()
            .map(|mob| crate::entity_storage::SavedEntity {
                id: mob.entity_type.clone(),
                uuid: mob.uuid,
                pos: mob.position(),
                motion: mob.velocity(),
                rotation: mob.rotation(),
                health: Some(mob.health),
                item: None,
                age: None,
                pickup_delay: None,
                extra: Vec::new(),
            })
            .collect();
        for (&id, state) in &self.item_state {
            let lifecycle = self.items.get(id).copied().unwrap_or_default();
            out.push(crate::entity_storage::SavedEntity {
                id: item_entity_type(),
                uuid: state.uuid,
                pos: state.motion.position,
                motion: state.motion.velocity,
                rotation: Rotation::new(0.0, 0.0),
                health: None,
                item: Some((state.item.clone(), lifecycle.count)),
                age: Some(lifecycle.age),
                pickup_delay: Some(lifecycle.pickup_delay),
                extra: Vec::new(),
            });
        }
        out
    }

    /// Snapshots the live population into the native typed vocabulary. This
    /// deliberately reuses [`saved_entities`](Self::saved_entities), the one
    /// disk view already kept coherent with the simulation, then removes only
    /// Anvil's opaque passthrough field which live entities never populate.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn native_entities(
        &self,
        dimension: lodestone_storage_schema::BuiltinDimension,
    ) -> Vec<crate::world_storage::NativeEntityRecord> {
        self.saved_entities()
            .into_iter()
            .filter_map(|saved| {
                let state = if saved.id == item_entity_type() {
                    let (item, count) = saved.item?;
                    Some(crate::world_storage::NativeEntityState::Item {
                        item,
                        count,
                        age: saved.age.unwrap_or(0),
                        pickup_delay: saved.pickup_delay.unwrap_or(0),
                    })
                } else {
                    Some(crate::world_storage::NativeEntityState::Living {
                        health: saved.health?,
                    })
                };
                Some(crate::world_storage::NativeEntityRecord {
                    uuid: *saved.uuid.as_bytes(),
                    entity_type: saved.id,
                    dimension,
                    position: saved.pos,
                    rotation: saved.rotation,
                    motion: saved.motion,
                    state,
                })
            })
            .collect()
    }

    /// Restores records from the native typed vocabulary through the same
    /// species/item constructors used by Anvil restoration. Pose-only records
    /// from older writers are skipped because they cannot distinguish a living
    /// body from a dropped item with enough state to restore safely.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore_native(
        &mut self,
        entities: &[crate::world_storage::NativeEntityRecord],
    ) -> usize {
        let saved: Vec<_> = entities
            .iter()
            .filter_map(|entity| {
                let (health, item, age, pickup_delay) = match &entity.state {
                    Some(crate::world_storage::NativeEntityState::Living { health }) => {
                        (Some(*health), None, None, None)
                    }
                    Some(crate::world_storage::NativeEntityState::Item {
                        item,
                        count,
                        age,
                        pickup_delay,
                    }) => (
                        None,
                        Some((item.clone(), *count)),
                        Some(*age),
                        Some(*pickup_delay),
                    ),
                    None => return None,
                };
                Some(crate::entity_storage::SavedEntity {
                    id: entity.entity_type.clone(),
                    uuid: Uuid::from_bytes(entity.uuid),
                    pos: entity.position,
                    motion: entity.motion,
                    rotation: entity.rotation,
                    health,
                    item,
                    age,
                    pickup_delay,
                    extra: Vec::new(),
                })
            })
            .collect();
        self.restore_saved(&saved)
    }

    /// Puts saved records back into the sim, returning how many were restored.
    ///
    /// A record whose `id` is `minecraft:item` becomes a tracked dropped item;
    /// anything else becomes a mob through [`spawn_species`](Self::spawn_species),
    /// so a restored cow gets the same shape, attributes and A\* budget a freshly
    /// spawned one does — the alternative (a bare position) would restore mobs
    /// that cannot path.
    ///
    /// **The stored uuid is reinstated, not regenerated**, because
    /// [`crate::entity_storage::EntityStorage::save`] clears stale records by
    /// uuid identity: a fresh uuid on load would make the next save unable to
    /// recognise its own entity, and the mob would be duplicated on every
    /// restart.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore_saved(&mut self, entities: &[crate::entity_storage::SavedEntity]) -> usize {
        let mut restored = 0usize;
        for saved in entities {
            if saved.id == item_entity_type() {
                let Some((item, count)) = saved.item.clone() else {
                    // An `Item`-less item entity is vanilla's own "empty stack"
                    // case, which it discards on load too.
                    continue;
                };
                let id = self.spawn_item(
                    item,
                    saved.pos,
                    saved.motion,
                    ItemLifecycle {
                        age: saved.age.unwrap_or(0),
                        pickup_delay: saved.pickup_delay.unwrap_or(0),
                        count: count.max(1),
                        ..ItemLifecycle::default()
                    },
                );
                if let Some(state) = self.item_state.get_mut(&id) {
                    state.uuid = saved.uuid;
                }
                restored += 1;
                continue;
            }
            // Checked **before** spawning, not after: a stored `0.0` is a mob
            // that died in the tick the process was killed, and spawning it to
            // then skip it would leave a zero-health corpse in the sim that
            // nothing sweeps, because the death pass only runs on damage.
            if saved.health.is_some_and(|health| health <= 0.0) {
                continue;
            }
            let mob = self.spawn_species(saved.id.clone(), saved.pos);
            mob.uuid = saved.uuid;
            mob.mob.set_body_yaw(saved.rotation.yaw);
            if let Some(health) = saved.health {
                mob.set_health(health);
            }
            restored += 1;
        }
        restored
    }
}
