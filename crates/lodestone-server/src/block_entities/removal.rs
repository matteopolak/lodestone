//! The one way a block entity leaves the registry with its block.
//!
//! [`BlockEntityRegistry::destroy_at`] is the only function that takes an
//! entity out of the map: the plain removal it wraps is private to
//! [`crate::block_entities`], so a site that clears a cell cannot also forget
//! to deal with what the cell held. The caller gets the entity back in a
//! [`Removal`] and decides what it drops (`crate::block_removal`).

use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ItemStack};

use super::{BlockEntity, BlockEntityRegistry};

/// A block entity taken out of the world with its block.
#[derive(Debug)]
pub(crate) struct Removal {
    pub(crate) pos: BlockPos,
    /// The block state the cell held before it changed.
    pub(crate) was: StateId,
    /// What the registry held there, if anything.
    pub(crate) entity: Option<BlockEntity>,
}

impl BlockEntityRegistry {
    /// Takes the entity at `pos` out of the registry because the block `was`
    /// there is gone, forgetting its lid viewers and waking comparators.
    pub(crate) fn destroy_at(&mut self, pos: BlockPos, was: StateId) -> Removal {
        self.lid_viewers.forget(pos);
        Removal { pos, was, entity: self.remove(pos) }
    }

    /// [`destroy_at`](Self::destroy_at) for a site with no way to spawn items
    /// (a reaction running over borrowed columns): the removal waits in the
    /// registry for the tick loop to drop it.
    pub(crate) fn destroy_deferred(&mut self, pos: BlockPos, was: StateId) {
        let removal = self.destroy_at(pos, was);
        self.deferred.push(removal);
    }

    /// The removals deferred since the last call, oldest first.
    pub(crate) fn take_deferred(&mut self) -> Vec<Removal> {
        std::mem::take(&mut self.deferred)
    }
}

impl BlockEntity {
    /// The stacks this entity spills when its block goes, whatever the cause:
    /// every slot of a container-shaped entity. A shulker box keeps its
    /// contents for the item it drops, and the stored-item blocks (pot,
    /// jukebox, bookshelf) are handled by `crate::stored_items`.
    #[must_use]
    pub(crate) fn spilled_contents(&self) -> Vec<ItemStack> {
        match self {
            BlockEntity::Container { id, slots } if id.name() != super::SHULKER_BOX_ID => slots.iter().flatten().cloned().collect(),
            BlockEntity::Furnace(_) | BlockEntity::Hopper(_) | BlockEntity::Lectern(_) | BlockEntity::Crafter { .. } => {
                self.container_slots().into_iter().flatten().collect()
            }
            BlockEntity::BrewingStand(stand) => [stand.ingredient(), stand.fuel_item()]
                .into_iter()
                .flatten()
                .filter_map(|(item, count)| Some(ItemStack::new(item.parse().ok()?, count)))
                .collect(),
            _ => Vec::new(),
        }
    }
}
