//! The NBT view of an entity, and the writes commands make through it.
//!
//! [`MobSim::entity_nbt`] is the one function producing an entity's NBT: the
//! save path's own record ([`MobSim::saved_entities`]) encoded by
//! [`SavedEntity::to_nbt`], so what `/data get entity` and `nbt=` read is what a
//! save writes and what `/summon` reads back. Entity kinds the save path does
//! not persist (boats, minecarts, primed TNT, bolts, the dragon, ...) get the
//! common header (`id`, `UUID`, `Pos`, `Motion`, `Rotation`, `Tags`) only.
//!
//! [`MobSim::apply_entity_nbt`] is the inverse for an entity that already
//! exists: it re-reads the record with [`SavedEntity::from_nbt`] and applies the
//! owned fields through the same restore code a world load uses, in place, so
//! the entity keeps its network id. Mobs apply everything the save path owns;
//! dropped items apply `Item`, `Age` and `PickupDelay`; every other kind applies
//! `Pos`, `Rotation` where it has one, and `Tags`.

use lodestone_core::Nbt;

use super::sim_command_entities::Located;
use super::*;
use crate::entity_record::SavedEntity;

/// Why [`MobSim::apply_entity_nbt`] changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityDataError {
    /// No live entity has that uuid.
    Missing,
    /// The compound is not a readable entity record (no `id` or `Pos`).
    Unreadable,
}

/// How a `/damage` hit ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DamageOutcome {
    /// The hit landed; `health` is what is left (`0.0` when it killed).
    Landed { health: f32 },
    /// The target took no damage: invulnerable, inside its invulnerability
    /// frames, or a kind that cannot be hurt.
    Unhurt,
    /// No live entity has that uuid.
    Missing,
}

impl<'w> MobSim<'w> {
    /// The record the save path would write for `uuid`, or a header-only one
    /// for a kind it does not persist.
    fn entity_record(&self, uuid: Uuid) -> Option<SavedEntity> {
        if let Some(record) = self.saved_trees().into_iter().find(|r| r.uuid == uuid) {
            return Some(record);
        }
        self.command_entities().into_iter().find(|e| e.uuid == uuid).map(|e| self.header_record(&e))
    }

    fn header_record(&self, entity: &CommandEntity) -> SavedEntity {
        let mut record = SavedEntity {
            id: entity.entity_type.clone(),
            uuid: entity.uuid,
            pos: entity.position,
            motion: Vec3::new(0.0, 0.0, 0.0),
            rotation: entity.rotation,
            health: None,
            item: None,
            age: None,
            pickup_delay: None,
            extra: Vec::new(),
        };
        self.attach_tags(&mut record);
        record
    }

    /// The NBT of the entity `uuid`; `None` when no such entity is live.
    #[must_use]
    pub fn entity_nbt(&self, uuid: Uuid) -> Option<Nbt> {
        self.entity_record(uuid).map(|record| record.to_nbt())
    }

    /// [`entity_nbt`](Self::entity_nbt) for every live entity, built in one
    /// pass for `nbt=` selector filtering.
    #[must_use]
    pub fn entity_nbt_table(&self) -> HashMap<Uuid, Nbt> {
        let mut table: HashMap<Uuid, Nbt> =
            self.saved_trees().into_iter().map(|record| (record.uuid, record.to_nbt())).collect();
        for entity in self.command_entities() {
            table.entry(entity.uuid).or_insert_with(|| self.header_record(&entity).to_nbt());
        }
        table
    }

    /// Applies `nbt` to the live entity `uuid` in place; see the module doc for
    /// what each kind takes. The entity keeps its uuid and type whatever the
    /// record says.
    ///
    /// # Errors
    ///
    /// [`EntityDataError`] when the entity is gone or the record unreadable.
    pub fn apply_entity_nbt(&mut self, uuid: Uuid, nbt: &Nbt) -> Result<(), EntityDataError> {
        let saved = SavedEntity::from_nbt(nbt).ok_or(EntityDataError::Unreadable)?;
        let located = self.locate(uuid).ok_or(EntityDataError::Missing)?;
        match located {
            Located::Mob(id) => self.apply_mob_record(id, &saved),
            Located::Item(id) => {
                self.apply_item_record(id, &saved);
                self.teleport_entity(uuid, saved.pos, None);
            }
            _ => {
                self.teleport_entity(uuid, saved.pos, Some(saved.rotation.yaw));
            }
        }
        self.set_entity_tags(uuid, &saved.extra);
        Ok(())
    }

    fn apply_mob_record(&mut self, id: i32, saved: &SavedEntity) {
        let Some(mob) = self.get_mut(id) else { return };
        if saved.pos != mob.position() {
            mob.teleport_to(saved.pos);
        }
        mob.mob.set_body_yaw(saved.rotation.yaw);
        if saved.motion != mob.velocity() {
            mob.apply_knockback(saved.motion);
        }
        if let Some(health) = saved.health {
            mob.set_health(health);
        }
        // The record is the whole truth about effects, so what it no longer lists goes.
        mob.effects.clear();
        super::sim_persistence::restore_growth(mob, &saved.extra);
        let pending = self.restore_mob_state(id, &saved.extra);
        self.resolve_references(vec![pending]);
        self.reap_dead();
    }

    fn apply_item_record(&mut self, id: i32, saved: &SavedEntity) {
        if let Some(stack) = &saved.item
            && let Some(state) = self.item_state.get_mut(&id)
        {
            state.item = stack.item.clone();
            state.components = (stack.components != Default::default())
                .then(|| std::sync::Arc::new(stack.components.clone()));
        }
        let Some(tracked) = self.items.remove(id) else { return };
        let count = saved
            .item
            .as_ref()
            .map_or(tracked.lifecycle.count, |stack| u8::try_from(stack.count).unwrap_or(u8::MAX).max(1));
        self.items.spawn(
            id,
            ItemLifecycle {
                count,
                age: saved.age.unwrap_or(tracked.lifecycle.age),
                pickup_delay: saved.pickup_delay.unwrap_or(tracked.lifecycle.pickup_delay),
                ..tracked.lifecycle
            },
        );
    }

    /// Replaces `uuid`'s tags with the `Tags` list in `fields`.
    fn set_entity_tags(&mut self, uuid: Uuid, fields: &[(String, Nbt)]) {
        let wanted: Vec<String> = match fields.iter().find(|(name, _)| name == "Tags") {
            Some((_, Nbt::List { elements, .. })) => elements
                .iter()
                .filter_map(|e| if let Nbt::String(tag) = e { Some(tag.clone()) } else { None })
                .collect(),
            _ => Vec::new(),
        };
        for held in self.entity_tags(uuid) {
            if !wanted.contains(&held) {
                self.remove_entity_tag(uuid, &held);
            }
        }
        for tag in wanted {
            self.add_entity_tag(uuid, &tag);
        }
    }

    /// Hurts the entity `uuid` for `amount` through its real damage pipeline:
    /// invulnerability frames, armour, resistance and enchantment reduction,
    /// absorption, per `flags`. `from` is the source position knockback is
    /// directed away from (unset for a sourceless type or `no_knockback`).
    /// `bypasses_invulnerability` lets the hit through an invulnerable mob.
    pub fn damage_entity(
        &mut self,
        uuid: Uuid,
        amount: f32,
        flags: DamageFlags,
        from: Option<Vec3>,
        bypasses_invulnerability: bool,
    ) -> DamageOutcome {
        let Some(located) = self.locate(uuid) else { return DamageOutcome::Missing };
        match located {
            Located::Mob(id) => {
                let Some(mob) = self.get_mut(id) else { return DamageOutcome::Missing };
                let was_invulnerable = mob.invulnerable;
                if bypasses_invulnerability {
                    mob.invulnerable = false;
                }
                let landed = match from {
                    Some(source) => {
                        let hit = self.attack(id, source, amount, flags, 0.0);
                        hit.is_some_and(|hit| hit.damage_dealt > 0.0 || hit.killed)
                    }
                    None => {
                        let landed = self.get_mut(id).and_then(|mob| mob.try_apply_damage(amount, flags)).is_some();
                        self.reap_dead();
                        landed
                    }
                };
                if let Some(mob) = self.get_mut(id) {
                    mob.invulnerable = was_invulnerable;
                }
                if landed {
                    DamageOutcome::Landed { health: self.get(id).map_or(0.0, SimMob::health) }
                } else {
                    DamageOutcome::Unhurt
                }
            }
            Located::Dragon(id) => match self.damage_dragon(id, amount) {
                Some(health) => DamageOutcome::Landed { health },
                None => DamageOutcome::Unhurt,
            },
            Located::Wither(id) => match self.damage_wither(id, amount, false, bypasses_invulnerability) {
                Some(health) => DamageOutcome::Landed { health },
                None => DamageOutcome::Unhurt,
            },
            Located::Crystal(id) => {
                if self.destroy_end_crystal(id).is_some() {
                    DamageOutcome::Landed { health: 0.0 }
                } else {
                    DamageOutcome::Unhurt
                }
            }
            _ => DamageOutcome::Unhurt,
        }
    }
}
