//! The mob side of bees and hives: bees entering a hive, leaving it, and tending
//! crops. The hive itself is a block entity ([`crate::beehive`]); this module
//! moves bees between the simulation and it, and the tick loop applies the world
//! writes.

use std::collections::HashMap;

use lodestone_data::block_states::StateId;

use super::*;
use crate::beehive::{MAX_OCCUPANTS, Occupant};

/// A bee that went into a hive this tick, already removed from the simulation.
#[derive(Debug, Clone)]
pub struct HiveEntry {
    /// The hive block.
    pub pos: BlockPos,
    /// The bee, saved.
    pub occupant: Occupant,
    /// The bloom the bee last knew.
    pub flower: Option<BlockPos>,
}

impl<'w> MobSim<'w> {
    /// Replaces the hive map the bees navigate by: hive block to occupant count.
    pub fn set_hives(&mut self, hives: HashMap<(i32, i32, i32), u8>) {
        self.hives = std::sync::Arc::new(hives);
    }

    /// Whether the sky keeps bees in their hives right now.
    #[must_use]
    pub fn bees_stay_in_hive(&self) -> bool {
        self.sky.bees_stay_in_hive(self.day_time)
    }

    /// Drains the bees that entered a hive.
    pub fn take_hive_entries(&mut self) -> Vec<HiveEntry> {
        std::mem::take(&mut self.pending_hive_entries)
    }

    /// Drains the crop states bees grew, for the tick loop to write.
    pub fn take_crop_growths(&mut self) -> Vec<(BlockPos, StateId)> {
        std::mem::take(&mut self.pending_crop_growths)
    }

    /// Turns each `(mob id, hive)` entry into a [`HiveEntry`] and removes the
    /// bee. A hive that is gone or already holds three bees refuses the bee,
    /// which keeps flying.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn resolve_hive_entries(&mut self, entries: Vec<(i32, (i32, i32, i32))>) {
        for (id, cell) in entries {
            let Some(&held) = self.hives.get(&cell) else { continue };
            if usize::from(held) >= MAX_OCCUPANTS {
                continue;
            }
            let Some(mob) = self.mobs.iter().find(|m| m.id == id) else { continue };
            let entity_data = self.saved_mob(mob).to_nbt();
            let flower = mob.mob.bee_state().flower.map(|(x, y, z)| BlockPos::new(x, y, z));
            self.remove_mob(id);
            std::sync::Arc::make_mut(&mut self.hives).insert(cell, held + 1);
            self.pending_hive_entries.push(HiveEntry {
                pos: BlockPos::new(cell.0, cell.1, cell.2),
                occupant: Occupant::entering(entity_data),
                flower,
            });
        }
    }

    /// Without persistence a bee cannot be stored, so it stays out.
    #[cfg(target_arch = "wasm32")]
    pub(super) fn resolve_hive_entries(&mut self, _entries: Vec<(i32, (i32, i32, i32))>) {}

    /// Puts a bee back into the world beside `hive`, which faces `facing`.
    ///
    /// The bee leaves the hive centre, offset past the hive's face unless that
    /// face is blocked. Its age and love timers run down by its time inside,
    /// it adopts the hive as home, and a bee that delivered nectar drops it.
    /// Returns whether the saved bee could be restored.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn release_bee(
        &mut self,
        hive: BlockPos,
        facing: (i32, i32),
        front_blocked: bool,
        released: &crate::beehive::Released,
        hive_flower: Option<BlockPos>,
        adopt_flower: bool,
    ) -> bool {
        let occupant = &released.occupant;
        let Some(mut saved) = crate::entity_storage::SavedEntity::from_nbt(&occupant.entity_data) else {
            return false;
        };
        saved.pos = Vec3::new(f64::from(hive.x) + 0.5, f64::from(hive.y) + 0.5, f64::from(hive.z) + 0.5);
        let uuid = saved.uuid;
        if self.restore_saved(std::slice::from_ref(&saved)) == 0 {
            return false;
        }
        let ticks = occupant.ticks_in_hive;
        let Some(mob) = self.mobs.iter_mut().find(|m| m.uuid == uuid) else { return false };
        let (width, height) = (f64::from(mob.mob.shape().width), f64::from(mob.mob.shape().height));
        let delta = if front_blocked { 0.0 } else { 0.55 + width / 2.0 };
        mob.teleport_to(Vec3::new(
            f64::from(hive.x) + 0.5 + delta * f64::from(facing.0),
            f64::from(hive.y) + 0.5 - height / 2.0,
            f64::from(hive.z) + 0.5 + delta * f64::from(facing.1),
        ));
        if !mob.mob.is_age_locked() {
            let age = mob.age();
            if age < 0 {
                mob.set_age((age + ticks).min(0));
            } else if age > 0 {
                mob.set_age((age - ticks).max(0));
            }
        }
        let love = mob.mob.love_time();
        mob.mob.set_love_time((love - ticks).max(0));
        let bee = mob.mob.bee_state_mut();
        bee.hive = Some((hive.x, hive.y, hive.z));
        if bee.flower.is_none()
            && let Some(flower) = hive_flower
            && adopt_flower
        {
            bee.flower = Some((flower.x, flower.y, flower.z));
        }
        if released.honey_delivered {
            bee.drop_off_nectar();
        }
        true
    }

    /// Without entity persistence a stored bee cannot be restored.
    #[cfg(target_arch = "wasm32")]
    pub fn release_bee(
        &mut self,
        _hive: BlockPos,
        _facing: (i32, i32),
        _front_blocked: bool,
        _released: &crate::beehive::Released,
        _hive_flower: Option<BlockPos>,
        _adopt_flower: bool,
    ) -> bool {
        false
    }
}

/// The horizontal step a hive's front face looks along.
#[must_use]
pub fn hive_facing(state: StateId) -> (i32, i32) {
    match state.properties().iter().find(|&&(k, _)| k == "facing").map(|&(_, v)| v) {
        Some("north") => (0, -1),
        Some("south") => (0, 1),
        Some("west") => (-1, 0),
        Some("east") => (1, 0),
        _ => (0, 1),
    }
}

/// The hive's honey level, or `None` for a block with no such property.
#[must_use]
pub fn honey_level(state: StateId) -> Option<u8> {
    state.properties().iter().find(|&&(k, _)| k == "honey_level").and_then(|&(_, v)| v.parse().ok())
}

/// `state` with its honey level replaced.
#[must_use]
pub fn with_honey_level(state: StateId, level: u8) -> Option<StateId> {
    let mut parts: std::collections::BTreeMap<String, String> =
        state.properties().iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect();
    parts.insert("honey_level".to_owned(), level.to_string());
    StateId::from_exact_parts(state.block().name(), &parts)
}
