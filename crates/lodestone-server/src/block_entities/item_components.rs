//! The components a block entity trades with its item: what a broken block's
//! entity contributes to the item it drops, and what a placed item restores
//! into the entity it makes.
//!
//! Only a hive carries any today: its bees travel as `minecraft:bees`. A new
//! kind adds its arm to both functions, and the matching name to
//! `crate::loot`'s copyable components so a table that copies it is bundled.

use lodestone_model::{BeeOccupant, ItemComponents};

use super::BlockEntity;
use crate::beehive::{Beehive, Occupant};

impl BlockEntity {
    /// The components this entity puts on the item its block drops.
    #[must_use]
    pub(crate) fn implicit_components(&self) -> ItemComponents {
        let mut components = ItemComponents::default();
        if let BlockEntity::Beehive(hive) = self {
            components.bees = hive
                .occupants()
                .iter()
                .filter_map(|bee| {
                    Some(BeeOccupant {
                        entity_data: crate::item_nbt::custom_data_from_nbt(&bee.entity_data)?,
                        ticks_in_hive: bee.ticks_in_hive,
                        min_ticks_in_hive: bee.min_ticks_in_hive,
                    })
                })
                .collect();
        }
        components
    }

    /// Restores what `components` (the placed item's) carry into this fresh
    /// entity.
    pub(crate) fn apply_implicit_components(&mut self, components: &ItemComponents) {
        if let BlockEntity::Beehive(hive) = self {
            let occupants = components
                .bees
                .iter()
                .filter_map(|bee| {
                    Some(Occupant {
                        entity_data: crate::item_nbt::custom_data_to_nbt(&bee.entity_data)?,
                        ticks_in_hive: bee.ticks_in_hive,
                        min_ticks_in_hive: bee.min_ticks_in_hive,
                    })
                })
                .collect();
            *hive = Beehive::restore(occupants, None);
        }
    }
}
