//! Analog redstone outputs that belong to entities rather than blocks.
//!
//! A comparator reads a block's analog output from its state or block entity;
//! some entities (an item frame) also have one, found by the comparator at the
//! cell an entity hangs in, on the face it points away from. This table is the
//! redstone side of that: entity sources publish `(cell, facing, strength)` as
//! it changes and the comparator input query reads it back, so the query never
//! reaches into the entity simulation.

use std::collections::HashMap;

use lodestone_model::BlockPos;

use crate::neighbor_update::Direction;

/// Where an entity-backed source reports its output: the cell it occupies and
/// the way it points.
type Slot = (BlockPos, Direction);

/// The entity-backed analog outputs of a world, keyed by source entity.
#[derive(Debug, Default)]
pub struct EntityAnalogOutputs {
    by_slot: HashMap<Slot, Vec<(i32, u8)>>,
    slot_of: HashMap<i32, Slot>,
}

impl EntityAnalogOutputs {
    /// Sets entity `id`'s output, or removes it with `None`.
    pub fn publish(&mut self, id: i32, output: Option<(BlockPos, Direction, u8)>) {
        let before = self.slot_of.get(&id).copied();
        if let Some(slot) = before {
            if let Some(sources) = self.by_slot.get_mut(&slot) {
                sources.retain(|(source, _)| *source != id);
                if sources.is_empty() {
                    self.by_slot.remove(&slot);
                }
            }
            self.slot_of.remove(&id);
        }
        if let Some((cell, facing, strength)) = output {
            self.by_slot.entry((cell, facing)).or_default().push((id, strength.min(15)));
            self.slot_of.insert(id, (cell, facing));
        }
    }

    /// The output a comparator facing `facing` finds in `cell`: the single
    /// source there pointing the same way. Two sources on one face are
    /// ambiguous and read as none, as does an empty face.
    #[must_use]
    pub fn read(&self, cell: BlockPos, facing: Direction) -> Option<u8> {
        match self.by_slot.get(&(cell, facing))?.as_slice() {
            [(_, strength)] => Some(*strength),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_source_reads_and_two_are_ambiguous() {
        let cell = BlockPos::new(1, 2, 3);
        let mut table = EntityAnalogOutputs::default();
        assert_eq!(table.read(cell, Direction::East), None);
        table.publish(7, Some((cell, Direction::East, 5)));
        assert_eq!(table.read(cell, Direction::East), Some(5));
        assert_eq!(table.read(cell, Direction::West), None, "another facing is another slot");
        table.publish(8, Some((cell, Direction::East, 9)));
        assert_eq!(table.read(cell, Direction::East), None, "two sources on one face are ambiguous");
        table.publish(8, None);
        assert_eq!(table.read(cell, Direction::East), Some(5));
        table.publish(7, Some((cell, Direction::East, 6)));
        assert_eq!(table.read(cell, Direction::East), Some(6), "a republish replaces the old strength");
        table.publish(7, None);
        assert!(table.slot_of.is_empty() && table.by_slot.is_empty());
    }
}
