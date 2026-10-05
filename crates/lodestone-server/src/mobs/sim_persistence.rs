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
    /// Arrows, tridents and thrown items are included, written by
    /// [`saved_projectiles`](Self::saved_projectiles). They reach only the
    /// Anvil record path: [`native_entities`](Self::native_entities) keeps
    /// records with a health or an item stack and skips them.
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
                extra: {
                    let mut fields = growth_fields(mob);
                    fields.extend(self.mob_state_fields(mob));
                    fields
                },
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
        out.extend(self.saved_projectiles());
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
                // A mob that died in the tick of the save has no valid health
                // to store and is not restored either.
                if saved.health.is_some_and(|health| health <= 0.0) {
                    return None;
                }
                let state = crate::world_storage::NativeEntityState {
                    health: saved.health,
                    item: saved.item,
                    age: saved.age,
                    pickup_delay: saved.pickup_delay,
                    fields: saved.extra,
                };
                if state.is_empty() {
                    return None;
                }
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
    /// constructors Anvil restoration uses, so a native record carries exactly
    /// the state an Anvil record would. Pose-only records from older writers
    /// are skipped because they cannot distinguish a living body from a dropped
    /// item with enough state to restore safely.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore_native(
        &mut self,
        entities: &[crate::world_storage::NativeEntityRecord],
    ) -> usize {
        let saved: Vec<_> = entities
            .iter()
            .filter(|entity| !entity.state.is_empty())
            .map(|entity| crate::entity_storage::SavedEntity {
                id: entity.entity_type.clone(),
                uuid: Uuid::from_bytes(entity.uuid),
                pos: entity.position,
                motion: entity.motion,
                rotation: entity.rotation,
                health: entity.state.health,
                item: entity.state.item.clone(),
                age: entity.state.age,
                pickup_delay: entity.state.pickup_delay,
                extra: entity.state.fields.clone(),
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
        let mut pending = Vec::new();
        // Projectiles resolve their shooter by uuid, so they restore after
        // every mob in the batch exists.
        let mut projectiles = Vec::new();
        for saved in entities {
            if matches!(
                saved.id.path(),
                "arrow" | "spectral_arrow" | "trident" | "snowball" | "egg" | "ender_pearl"
                    | "splash_potion" | "lingering_potion" | "experience_bottle"
            ) {
                projectiles.push(saved);
                continue;
            }
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
            restore_growth(mob, &saved.extra);
            let id = mob.id;
            pending.push(self.restore_mob_state(id, &saved.extra));
            restored += 1;
        }
        self.resolve_references(pending);
        for saved in projectiles {
            if self.restore_projectile(saved) {
                restored += 1;
            }
        }
        restored
    }
}

/// The ageable-mob growth fields vanilla writes: an `Int` age timer (negative
/// while a baby, positive as the post-breeding cooldown) and the
/// golden-dandelion lock. Written only when either differs from its default,
/// which is lossless because a missing field reads back as that default.
#[cfg(not(target_arch = "wasm32"))]
fn growth_fields(mob: &SimMob<'_>) -> Vec<(String, lodestone_core::Nbt)> {
    use lodestone_core::Nbt;
    let mut fields = Vec::new();
    if mob.age() != 0 {
        fields.push(("Age".to_owned(), Nbt::Int(mob.age())));
    }
    if mob.mob.is_age_locked() {
        fields.push(("AgeLocked".to_owned(), Nbt::Byte(1)));
    }
    fields
}

/// Applies [`growth_fields`] back to a freshly spawned mob. Goes through
/// [`SimMob::set_age`] so a restored baby also gets its baby hitbox and step.
#[cfg(not(target_arch = "wasm32"))]
fn restore_growth(mob: &mut SimMob<'_>, extra: &[(String, lodestone_core::Nbt)]) {
    use lodestone_core::Nbt;
    for (name, value) in extra {
        match (name.as_str(), value) {
            ("Age", Nbt::Int(age)) => {
                mob.set_age(*age);
            }
            ("AgeLocked", Nbt::Byte(locked)) => {
                mob.mob.set_age_locked(*locked != 0);
            }
            _ => {}
        }
    }
}
