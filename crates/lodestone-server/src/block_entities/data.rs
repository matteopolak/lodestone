//! The NBT view of a registered block entity and the edits `/data` makes through it.
//!
//! [`BlockEntityRegistry::entity_nbt`] is the chunk save path's own record
//! ([`block_entity_to_nbt`]), and [`BlockEntityRegistry::apply_entity_nbt`]
//! reads an edited record back with the loader a chunk load uses
//! ([`block_entity_from_nbt`]), so a command sees and writes exactly what a save
//! holds. The write goes through [`insert`](BlockEntityRegistry::insert), which
//! wakes comparators, and queues the position for the tick loop to send the
//! client its data-update packet when the kind streams one (a sign's text, a
//! spawner's state; containers stream nothing).

use lodestone_core::Nbt;
use lodestone_model::BlockPos;

use super::BlockEntityRegistry;
use crate::chunk_nbt::{block_entity_from_nbt, block_entity_to_nbt, block_entity_update_nbt};

/// Why [`BlockEntityRegistry::apply_entity_nbt`] changed nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockDataError {
    /// No block entity is registered at the position.
    Missing,
    /// The compound is not a record the loader reads for this position.
    Unreadable,
}

impl BlockEntityRegistry {
    /// The save-form compound of the entity at `pos`, with `id`, `x`, `y`, `z`.
    #[must_use]
    pub fn entity_nbt(&self, pos: BlockPos) -> Option<Nbt> {
        self.get(pos).map(|entity| block_entity_to_nbt(pos, entity))
    }

    /// Replaces the entity at `pos` with the record `nbt` describes. The entity
    /// keeps its position and type whatever the record says.
    ///
    /// # Errors
    ///
    /// [`BlockDataError`] when nothing is registered at `pos` or the loader
    /// cannot read the record.
    pub fn apply_entity_nbt(&mut self, pos: BlockPos, nbt: &Nbt) -> Result<(), BlockDataError> {
        let current = self.entity_nbt(pos).ok_or(BlockDataError::Missing)?;
        let (Nbt::Compound(held), Nbt::Compound(incoming)) = (&current, nbt) else {
            return Err(BlockDataError::Unreadable);
        };
        let mut record = incoming.clone();
        record.retain(|(name, _)| !matches!(name.as_str(), "id" | "x" | "y" | "z"));
        record.extend(held.iter().filter(|(name, _)| matches!(name.as_str(), "id" | "x" | "y" | "z")).cloned());
        let (_, entity) = block_entity_from_nbt(&Nbt::Compound(record)).ok_or(BlockDataError::Unreadable)?;
        if self.entities.get(&pos).is_some_and(crate::container_signal::carries_analog) {
            self.analog_dirty.insert(pos);
        }
        if block_entity_update_nbt(pos, &entity) != Nbt::End {
            self.client_dirty.insert(pos);
        }
        self.insert(pos, entity);
        Ok(())
    }

    /// The positions whose client-visible data changed since the last call, in a
    /// fixed order, clearing the set; each with the update packet's tag.
    pub fn take_client_dirty(&mut self) -> Vec<(BlockPos, super::BlockEntityKind, Nbt)> {
        let mut dirty: Vec<BlockPos> = self.client_dirty.drain().collect();
        dirty.sort_unstable_by_key(|pos| (pos.x, pos.y, pos.z));
        dirty
            .into_iter()
            .filter_map(|pos| {
                let entity = self.entities.get(&pos)?;
                Some((pos, entity.kind(), block_entity_update_nbt(pos, entity)))
            })
            .collect()
    }
}
