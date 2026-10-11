//! Persistence and despawn-facing entity lifecycle operations.

use std::collections::HashSet;

use super::sim_command_entities::Located;
use super::*;
use crate::entity_record::SavedEntity;

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
    /// [`saved_projectiles`](Self::saved_projectiles), and reach both record
    /// paths: their saved fields are the state
    /// [`native_entities`](Self::native_entities) keeps.
    ///
    /// A mob riding a saved vehicle is nested in the vehicle's record as a
    /// `Passengers` list rather than listed beside it, as the reference save
    /// format does; players are saved on their own.
    #[must_use]
    pub fn saved_entities(&self) -> Vec<crate::entity_record::SavedEntity> {
        let (flat, links) = self.flat_saved_entities();
        let riders: HashSet<Uuid> = links.iter().map(|&(rider, _)| rider).collect();
        let by_uuid: HashMap<Uuid, SavedEntity> = flat.iter().map(|r| (r.uuid, r.clone())).collect();
        flat.into_iter()
            .filter(|record| !riders.contains(&record.uuid))
            .map(|record| with_passengers(record, &by_uuid, &links))
            .collect()
    }

    /// Every saved record with its `Passengers` nested, riders included as
    /// records of their own: the view `/data get` and `nbt=` read.
    pub(super) fn saved_trees(&self) -> Vec<crate::entity_record::SavedEntity> {
        let (flat, links) = self.flat_saved_entities();
        let by_uuid: HashMap<Uuid, SavedEntity> = flat.iter().map(|r| (r.uuid, r.clone())).collect();
        flat.into_iter().map(|record| with_passengers(record, &by_uuid, &links)).collect()
    }

    /// The saved records with no nesting, and the `(rider, vehicle)` uuid links
    /// of the mobs that ride a saved vehicle, in boarding order.
    fn flat_saved_entities(&self) -> (Vec<SavedEntity>, Vec<(Uuid, Uuid)>) {
        let mut out: Vec<crate::entity_record::SavedEntity> =
            self.mobs.iter().map(|mob| self.saved_mob(mob)).collect();
        for (&id, state) in &self.item_state {
            let lifecycle = self.items.get(id).copied().unwrap_or_default();
            out.push(crate::entity_record::SavedEntity {
                id: item_entity_type(),
                uuid: state.uuid,
                pos: state.motion.position,
                motion: state.motion.velocity,
                rotation: Rotation::new(0.0, 0.0),
                health: None,
                item: Some(state.stack(lifecycle.count)),
                age: Some(lifecycle.age),
                pickup_delay: Some(lifecycle.pickup_delay),
                extra: Vec::new(),
            });
        }
        out.extend(self.saved_projectiles());
        out.extend(self.saved_cushions());
        out.extend(self.saved_frames());
        out.extend(self.saved_vehicles());
        for record in &mut out {
            self.attach_tags(record);
        }
        let saved: HashSet<Uuid> = out.iter().map(|r| r.uuid).collect();
        let links = self
            .riders
            .iter()
            .filter(|ride| !ride.player)
            .filter_map(|ride| Some((self.seat_uuid(ride.rider)?, self.seat_uuid(ride.vehicle)?)))
            .filter(|(rider, vehicle)| saved.contains(rider) && saved.contains(vehicle))
            .collect();
        (out, links)
    }

    /// The uuid of the mob, boat, minecart or cushion `id`.
    fn seat_uuid(&self, id: i32) -> Option<Uuid> {
        self.get(id)
            .map(|m| m.uuid)
            .or_else(|| self.vehicles.get(&id).map(|v| v.uuid))
            .or_else(|| self.minecarts.get(&id).map(|c| c.uuid))
            .or_else(|| self.cushions.get(&id).map(|c| c.uuid))
    }

    /// Boats, rafts and minecarts: type, uuid, position, motion and yaw, plus a
    /// cart's own fields (`TrackedMinecart::saved_fields`).
    fn saved_vehicles(&self) -> Vec<SavedEntity> {
        let record = |id: ResourceKey,
                      uuid: Uuid,
                      motion: &lodestone_physics::EntityMotion,
                      yaw: f32,
                      extra: Vec<(String, lodestone_core::Nbt)>| SavedEntity {
            id,
            uuid,
            pos: Vec3::new(motion.position.x, motion.position.y, motion.position.z),
            motion: Vec3::new(motion.velocity.x, motion.velocity.y, motion.velocity.z),
            rotation: Rotation::new(yaw, 0.0),
            health: None,
            item: None,
            age: None,
            pickup_delay: None,
            extra,
        };
        let mut boats: Vec<_> = self.vehicles.iter().collect();
        boats.sort_unstable_by_key(|&(id, _)| *id);
        let mut carts: Vec<_> = self.minecarts.iter().collect();
        carts.sort_unstable_by_key(|&(id, _)| *id);
        boats
            .into_iter()
            .map(|(_, v)| record(v.entity_type.clone(), v.uuid, &v.motion, v.yaw, Vec::new()))
            .chain(carts.into_iter().map(|(_, c)| record(c.kind.entity_type(), c.uuid, &c.motion, c.yaw, c.saved_fields())))
            .collect()
    }

    /// Re-creates a saved boat or minecart with its stored uuid; `false` when
    /// the record is neither.
    fn restore_vehicle(&mut self, saved: &SavedEntity) -> bool {
        let at = lodestone_physics::Vec3d::new(saved.motion.x, saved.motion.y, saved.motion.z);
        if crate::boat::is_boat_type_path(saved.id.path()) {
            let id = self.spawn_vehicle(saved.id.clone(), saved.pos, saved.rotation.yaw);
            if let Some(boat) = self.vehicles.get_mut(&id) {
                boat.uuid = saved.uuid;
                boat.motion.velocity = at;
            }
            return true;
        }
        let Some(kind) = minecart::MinecartKind::from_entity(&saved.id.to_string()) else { return false };
        let id = self.spawn_minecart(kind, saved.pos);
        if let Some(cart) = self.minecarts.get_mut(&id) {
            cart.restore_fields(&saved.extra);
            cart.uuid = saved.uuid;
            cart.yaw = saved.rotation.yaw;
            cart.yaw_o = saved.rotation.yaw;
            cart.motion.velocity = at;
        }
        true
    }

    /// Restores the `Passengers` nested in `parent`, then seats them on it.
    fn restore_passengers(&mut self, parent: &SavedEntity) -> usize {
        let Some((_, lodestone_core::Nbt::List { elements, .. })) =
            parent.extra.iter().find(|(name, _)| name == "Passengers")
        else {
            return 0;
        };
        let children: Vec<SavedEntity> = elements.iter().filter_map(SavedEntity::from_nbt).collect();
        let restored = self.restore_saved(&children);
        let Some(vehicle) = self.entity_id_of(parent.uuid) else { return restored };
        for child in &children {
            if let Some(Located::Mob(rider)) = self.locate(child.uuid) {
                self.board(vehicle, rider, false, true);
            }
        }
        restored
    }

    /// Writes the entity's `/tag` tags into `record` as a `Tags` list, when it has any.
    pub(super) fn attach_tags(&self, record: &mut crate::entity_record::SavedEntity) {
        let tags = self.entity_tags(record.uuid);
        if tags.is_empty() {
            return;
        }
        record.extra.retain(|(name, _)| name != "Tags");
        record.extra.push((
            "Tags".to_owned(),
            lodestone_core::Nbt::List {
                element_type: lodestone_core::NbtTag::String,
                elements: tags.into_iter().map(lodestone_core::Nbt::String).collect(),
            },
        ));
    }

    /// One mob as the record [`crate::entity_storage`] persists.
    pub(super) fn saved_mob(&self, mob: &SimMob<'_>) -> crate::entity_record::SavedEntity {
        crate::entity_record::SavedEntity {
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
        }
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
                if state.is_empty() && !is_stateless_vehicle(&saved.id) {
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
            .filter(|entity| !entity.state.is_empty() || is_stateless_vehicle(&entity.entity_type))
            .map(|entity| crate::entity_record::SavedEntity {
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
    /// Restores one record through [`Self::restore_saved`] (so a command-built
    /// record and a loaded one cannot differ) and returns its `(uuid, network
    /// id)`, or `None` when the record was refused (a dead `Health`, an
    /// `Item`-less item).
    pub fn summon_saved(&mut self, saved: &crate::entity_record::SavedEntity) -> Option<(Uuid, i32)> {
        if self.restore_saved(std::slice::from_ref(saved)) == 0 {
            return None;
        }
        let id = self.entity_id_of(saved.uuid)?;
        if let Some(mob) = self.mobs.iter_mut().find(|m| m.uuid == saved.uuid) {
            mob.persistent = true;
        }
        Some((saved.uuid, id))
    }

    pub fn restore_saved(&mut self, entities: &[crate::entity_record::SavedEntity]) -> usize {
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
            if matches!(saved.id.path(), "item_frame" | "glow_item_frame") {
                self.restore_frame(saved);
                restored += 1;
                continue;
            }
            if (crate::boat::is_boat_type_path(saved.id.path())
                || minecart::MinecartKind::from_entity(&saved.id.to_string()).is_some())
                && self.restore_vehicle(saved)
            {
                restored += 1;
                continue;
            }
            if saved.id.path() == "cushion" {
                self.restore_cushion(saved);
                restored += 1;
                continue;
            }
            if saved.id == item_entity_type() {
                let Some(stack) = saved.item.clone() else {
                    // An `Item`-less item entity is vanilla's own "empty stack"
                    // case, which it discards on load too.
                    continue;
                };
                let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
                let id = self.spawn_item(
                    &stack,
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
            if saved.motion != Vec3::new(0.0, 0.0, 0.0) {
                mob.apply_knockback(saved.motion);
            }
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
        for saved in entities {
            let Some((_, lodestone_core::Nbt::List { elements, .. })) =
                saved.extra.iter().find(|(name, _)| name == "Tags")
            else {
                continue;
            };
            for element in elements {
                if let lodestone_core::Nbt::String(tag) = element {
                    self.add_entity_tag(saved.uuid, tag);
                }
            }
        }
        for saved in entities {
            restored += self.restore_passengers(saved);
        }
        restored
    }
}

/// Whether `id` is a vehicle whose whole saved form is its pose: an empty
/// boat or raft. Its record has no state fields, which elsewhere marks an
/// older pose-only record that cannot be restored.
fn is_stateless_vehicle(id: &ResourceKey) -> bool {
    crate::boat::is_boat_type_path(id.path())
}

/// `record` with the records riding it nested as a `Passengers` list, as the
/// reference save format writes them; riders of riders nest in turn.
fn with_passengers(
    mut record: SavedEntity,
    by_uuid: &HashMap<Uuid, SavedEntity>,
    links: &[(Uuid, Uuid)],
) -> SavedEntity {
    let riders: Vec<lodestone_core::Nbt> = links
        .iter()
        .filter(|&&(_, vehicle)| vehicle == record.uuid)
        .filter_map(|(rider, _)| by_uuid.get(rider))
        .map(|rider| with_passengers(rider.clone(), by_uuid, links).to_nbt())
        .collect();
    if !riders.is_empty() {
        record.extra.retain(|(name, _)| name != "Passengers");
        record.extra.push((
            "Passengers".to_owned(),
            lodestone_core::Nbt::List { element_type: lodestone_core::NbtTag::Compound, elements: riders },
        ));
    }
    record
}

/// The ageable-mob growth fields vanilla writes: an `Int` age timer (negative
/// while a baby, positive as the post-breeding cooldown) and the
/// golden-dandelion lock. Written only when either differs from its default,
/// which is lossless because a missing field reads back as that default.
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
pub(super) fn restore_growth(mob: &mut SimMob<'_>, extra: &[(String, lodestone_core::Nbt)]) {
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
