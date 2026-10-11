//! The command-facing view of every entity the sim owns, and the by-uuid
//! mutations commands apply to them.
//!
//! [`MobSim::command_entities`] is the one enumeration selectors resolve over:
//! it is built from [`MobSim::snapshots`], so a new entity kind that streams to
//! clients is selectable without a second registration. Anything that has to
//! act on an entity by uuid (`kill_entity`, `teleport_entity`, effects, tags)
//! lives here beside it.

use std::collections::BTreeSet;

use super::*;

/// One entity as a command selector sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandEntity {
    /// The network entity id.
    pub entity_id: i32,
    pub uuid: Uuid,
    /// The canonical entity type, e.g. `minecraft:zombie`.
    pub entity_type: ResourceKey,
    /// Feet position; a hanging frame reports its box centre.
    pub position: Vec3,
    pub rotation: Rotation,
    /// The custom name as plain text, `""` when unnamed.
    pub name: String,
    pub tags: Vec<String>,
}

/// What removing an entity left behind for the caller to finish.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EntityKill {
    /// Set for an item frame: the framed map must forget its marker.
    pub frame: Option<FrameBroken>,
}

/// Whether an entity could be moved by [`MobSim::teleport_entity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeleportOutcome {
    Moved,
    /// The entity exists but its kind keeps position-derived state (hanging
    /// frames, falling blocks, lightning, fishing bobbers, the dragon) that a
    /// move would corrupt.
    Unsupported,
}

/// Where an entity lives, so a by-uuid operation finds its store once.
pub(super) enum Located {
    Mob(i32),
    Projectile(i32),
    Item(i32),
    Orb(i32),
    Falling(i32),
    Vehicle(i32),
    Cushion(i32),
    Frame(i32),
    Tnt(i32),
    Eye(i32),
    Minecart(i32),
    Bolt(i32),
    Dragon(i32),
    Crystal(i32),
    Wither(i32),
    Bobber(i32),
}

impl<'w> MobSim<'w> {
    /// Every live entity, in network-id order.
    #[must_use]
    pub fn command_entities(&self) -> Vec<CommandEntity> {
        let mut out: Vec<CommandEntity> = self
            .snapshots()
            .into_iter()
            .map(|snapshot| {
                let name = snapshot
                    .metadata
                    .iter()
                    .find_map(|field| match field {
                        MetadataField::CustomName(Some(text)) => Some(text.to_plain_string()),
                        _ => None,
                    })
                    .unwrap_or_default();
                // A frame streams its attachment corner; selectors measure from its centre.
                let position = self.frames.get(&snapshot.id).map_or(snapshot.position, TrackedFrame::centre);
                CommandEntity {
                    entity_id: snapshot.id,
                    uuid: snapshot.uuid,
                    entity_type: snapshot.entity_type,
                    position,
                    rotation: snapshot.rotation,
                    name,
                    tags: self.entity_tags(snapshot.uuid),
                }
            })
            .collect();
        out.sort_by_key(|e| e.entity_id);
        out
    }

    /// The tags on `uuid`, sorted.
    #[must_use]
    pub fn entity_tags(&self, uuid: Uuid) -> Vec<String> {
        self.entity_tags.get(&uuid).map(|tags| tags.iter().cloned().collect()).unwrap_or_default()
    }

    /// Every entity's tags by uuid, for resolving a selector's `tag=`.
    #[must_use]
    pub fn tag_table(&self) -> HashMap<Uuid, Vec<String>> {
        self.entity_tags.iter().map(|(&uuid, tags)| (uuid, tags.iter().cloned().collect())).collect()
    }

    /// Adds `tag`; `false` when already present or the entity holds the
    /// reference limit of 1024 tags.
    pub fn add_entity_tag(&mut self, uuid: Uuid, tag: &str) -> bool {
        let tags = self.entity_tags.entry(uuid).or_default();
        tags.len() < MAX_ENTITY_TAGS && tags.insert(tag.to_owned())
    }

    /// Removes `tag`; `false` when it was absent.
    pub fn remove_entity_tag(&mut self, uuid: Uuid, tag: &str) -> bool {
        let Some(tags) = self.entity_tags.get_mut(&uuid) else { return false };
        let removed = tags.remove(tag);
        if tags.is_empty() {
            self.entity_tags.remove(&uuid);
        }
        removed
    }

    /// Drops tag entries whose entity is neither live in this sim nor in
    /// `keep` (the connected players, which the sim does not own).
    pub fn prune_entity_tags(&mut self, keep: &[Uuid]) {
        if self.entity_tags.is_empty() {
            return;
        }
        let live: BTreeSet<Uuid> =
            self.snapshots().into_iter().map(|s| s.uuid).chain(keep.iter().copied()).collect();
        self.entity_tags.retain(|uuid, _| live.contains(uuid));
    }

    /// Kills the entity `uuid` as `/kill` does: a mob dies through its normal
    /// death (loot included), a dragon or wither takes unblockable damage, and
    /// everything else is discarded. `None` when no such entity is live.
    pub fn kill_entity(&mut self, uuid: Uuid) -> Option<EntityKill> {
        let mut kill = EntityKill::default();
        match self.locate(uuid)? {
            Located::Mob(id) => {
                if let Some(mob) = self.get_mut(id) {
                    mob.set_health(0.0);
                }
                self.reap_dead();
            }
            Located::Projectile(id) => {
                self.remove_projectile(id);
            }
            Located::Item(id) => {
                self.remove_item(id);
            }
            Located::Orb(id) => {
                self.orbs.remove(&id);
                self.orb_handoff.forget_entity(id);
            }
            Located::Falling(id) => {
                self.falling_blocks.remove(&id);
            }
            Located::Vehicle(id) => {
                self.vehicles.remove(&id);
            }
            Located::Cushion(id) => {
                self.break_cushion(id, false);
            }
            Located::Frame(id) => kill.frame = self.kill_frame(id, false),
            Located::Tnt(id) => {
                self.tnt.remove(&id);
            }
            Located::Eye(id) => {
                self.eyes.remove(&id);
            }
            Located::Minecart(id) => {
                self.minecarts.remove(&id);
            }
            Located::Bolt(id) => {
                self.lightning_bolts.remove(&id);
            }
            Located::Dragon(id) => {
                self.damage_dragon(id, f32::MAX);
            }
            Located::Crystal(id) => {
                self.destroy_end_crystal(id);
            }
            Located::Wither(id) => {
                self.damage_wither(id, f32::MAX, false, true);
            }
            Located::Bobber(id) => {
                self.fishing_bobbers.remove(&id);
            }
        }
        Some(kill)
    }

    /// Moves the entity `uuid` to `position`, turning it to `yaw` where its kind
    /// has one. `None` when no such entity is live.
    pub fn teleport_entity(&mut self, uuid: Uuid, position: Vec3, yaw: Option<f32>) -> Option<TeleportOutcome> {
        let to_d = |p: Vec3| lodestone_physics::Vec3d { x: p.x, y: p.y, z: p.z };
        match self.locate(uuid)? {
            Located::Mob(id) => {
                let mob = self.get_mut(id)?;
                mob.teleport_to(position);
                if let Some(yaw) = yaw {
                    mob.mob.set_body_yaw(yaw);
                }
            }
            Located::Projectile(id) => {
                let mut tracked = self.projectiles.remove(id)?;
                tracked.projectile.position = position;
                self.projectiles.replace(tracked);
            }
            Located::Item(id) => {
                let state = self.item_state.get_mut(&id)?;
                state.motion.position = position;
                state.owner = ItemTickOwner::for_position(position);
                self.item_handoff.forget_entity(id);
            }
            Located::Orb(id) => {
                let orb = self.orbs.get_mut(&id)?;
                orb.motion.position = position;
                orb.owner = orbs::owner_key(position);
                self.orb_handoff.forget_entity(id);
            }
            Located::Vehicle(id) => {
                let vehicle = self.vehicles.get_mut(&id)?;
                vehicle.motion.position = to_d(position);
                if let Some(yaw) = yaw {
                    vehicle.yaw = yaw;
                }
            }
            Located::Cushion(id) => {
                let cushion = self.cushions.get_mut(&id)?;
                cushion.position = position;
                if let Some(yaw) = yaw {
                    cushion.yaw = yaw;
                }
            }
            Located::Tnt(id) => self.tnt.get_mut(&id)?.motion.position = to_d(position),
            Located::Eye(id) => self.eyes.get_mut(&id)?.position = position,
            Located::Minecart(id) => {
                let cart = self.minecarts.get_mut(&id)?;
                cart.motion.position = to_d(position);
                if let Some(yaw) = yaw {
                    cart.yaw = yaw;
                }
            }
            Located::Crystal(id) => self.crystals.get_mut(&id)?.position = position,
            Located::Wither(id) => {
                let wither = self.withers.get_mut(&id)?;
                wither.position = position;
                if let Some(yaw) = yaw {
                    wither.yaw = yaw;
                }
            }
            Located::Falling(_) | Located::Frame(_) | Located::Bolt(_) | Located::Dragon(_) | Located::Bobber(_) => {
                return Some(TeleportOutcome::Unsupported);
            }
        }
        Some(TeleportOutcome::Moved)
    }

    /// Gives a status effect to a living mob; `false` for a non-living entity,
    /// an unknown effect id, or an effect the mob already holds at least as strongly.
    pub fn apply_effect_to(&mut self, uuid: Uuid, effect: &str, duration: i32, amplifier: u32) -> bool {
        let Some(Located::Mob(id)) = self.locate(uuid) else { return false };
        self.get_mut(id).is_some_and(|mob| mob.apply_effect(effect, duration, amplifier))
    }

    /// Removes one effect (`Some`) or every effect (`None`) from a living mob;
    /// `false` when nothing was removed.
    pub fn clear_effect_of(&mut self, uuid: Uuid, effect: Option<&str>) -> bool {
        let Some(Located::Mob(id)) = self.locate(uuid) else { return false };
        let Some(mob) = self.get_mut(id) else { return false };
        match effect {
            Some(effect) => mob.effects.remove(effect),
            None => {
                let had = !mob.effects.is_empty();
                mob.effects.clear();
                had
            }
        }
    }

    /// The network id of the entity with `uuid`, whatever store holds it.
    pub(super) fn entity_id_of(&self, uuid: Uuid) -> Option<i32> {
        use Located::{
            Bobber, Bolt, Crystal, Cushion, Dragon, Eye, Falling, Frame, Item, Minecart, Mob, Orb,
            Projectile, Tnt, Vehicle, Wither,
        };
        match self.locate(uuid)? {
            Mob(id) | Projectile(id) | Item(id) | Orb(id) | Falling(id) | Vehicle(id) | Cushion(id)
            | Frame(id) | Tnt(id) | Eye(id) | Minecart(id) | Bolt(id) | Dragon(id) | Crystal(id)
            | Wither(id) | Bobber(id) => Some(id),
        }
    }

    pub(super) fn locate(&self, uuid: Uuid) -> Option<Located> {
        fn find<T>(map: &HashMap<i32, T>, uuid: Uuid, uuid_of: impl Fn(&T) -> Uuid) -> Option<i32> {
            map.iter().find(|(_, v)| uuid_of(v) == uuid).map(|(&id, _)| id)
        }
        if let Some(mob) = self.mobs.iter().find(|m| m.uuid == uuid) {
            return Some(Located::Mob(mob.id));
        }
        if let Some((&id, _)) = self.projectile_meta.iter().find(|(_, m)| m.uuid == uuid) {
            return Some(Located::Projectile(id));
        }
        find(&self.item_state, uuid, |s| s.uuid)
            .map(Located::Item)
            .or_else(|| find(&self.orbs, uuid, |o| o.uuid).map(Located::Orb))
            .or_else(|| find(&self.falling_blocks, uuid, |b| b.uuid).map(Located::Falling))
            .or_else(|| find(&self.vehicles, uuid, |v| v.uuid).map(Located::Vehicle))
            .or_else(|| find(&self.cushions, uuid, |c| c.uuid).map(Located::Cushion))
            .or_else(|| find(&self.frames, uuid, |f| f.uuid).map(Located::Frame))
            .or_else(|| find(&self.tnt, uuid, |t| t.uuid).map(Located::Tnt))
            .or_else(|| find(&self.eyes, uuid, |e| e.uuid).map(Located::Eye))
            .or_else(|| find(&self.minecarts, uuid, |c| c.uuid).map(Located::Minecart))
            .or_else(|| find(&self.lightning_bolts, uuid, |b| b.uuid).map(Located::Bolt))
            .or_else(|| find(&self.dragons, uuid, |d| d.uuid).map(Located::Dragon))
            .or_else(|| find(&self.crystals, uuid, |c| c.uuid).map(Located::Crystal))
            .or_else(|| find(&self.withers, uuid, |w| w.uuid).map(Located::Wither))
            .or_else(|| find(&self.fishing_bobbers, uuid, |b| b.uuid).map(Located::Bobber))
    }
}

/// The reference cap on tags per entity.
const MAX_ENTITY_TAGS: usize = 1024;
