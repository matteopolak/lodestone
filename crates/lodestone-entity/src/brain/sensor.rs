//! Sensors: the perception step that populates memory before behaviours run.
//!
//! Faithful to vanilla's `Sensor`. Each tick, before any behaviour starts, every
//! sensor writes what the mob currently perceives into memory
//! (`NEAREST_VISIBLE_PLAYER`, nearby entities, and so on). Behaviours then read
//! only memory, never the world directly — so the world coupling lives entirely
//! in sensors and the [`BrainMob`] seam.

use super::memory::{Memories, MemoryModuleType, MemoryValue};
use super::mob::BrainMob;

/// The perception units a brain ticks each frame.
pub trait Sensor {
    /// Updates memory from the mob's current perception.
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob);

    /// Every memory this sensor writes, so the brain can register it.
    fn output_memories(&self) -> Vec<MemoryModuleType>;

    /// A short name for debugging.
    fn name(&self) -> &'static str;
}

/// Writes [`MemoryModuleType::NEAREST_VISIBLE_PLAYER`] from the mob's nearest
/// visible player, clearing it when there is none.
#[derive(Debug, Default)]
pub struct NearestPlayerSensor;

impl Sensor for NearestPlayerSensor {
    fn tick(&mut self, mem: &mut Memories, mob: &mut dyn BrainMob) {
        match mob.nearest_visible_player() {
            Some(pos) => mem.set(
                MemoryModuleType::NEAREST_VISIBLE_PLAYER,
                MemoryValue::Pos(pos),
            ),
            None => mem.erase(MemoryModuleType::NEAREST_VISIBLE_PLAYER),
        }
    }

    fn output_memories(&self) -> Vec<MemoryModuleType> {
        vec![MemoryModuleType::NEAREST_VISIBLE_PLAYER]
    }

    fn name(&self) -> &'static str {
        "nearest_player"
    }
}
