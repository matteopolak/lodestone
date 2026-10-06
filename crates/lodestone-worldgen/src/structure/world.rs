//! Clipped mutable state views consumed by structure placement.

use lodestone_data::block_states::StateId;

use crate::dense_grid::BaseStateFacts;

use super::processor::WorldRead;

/// The state and mutation surface shared by dense and sparse structure worlds.
pub trait StructureWorld: WorldRead + std::fmt::Debug + Send {
    /// The origin and dimensions of the accepted write box.
    fn bounds(&self) -> (i32, i32, i32, i32, i32, i32);
    fn get_id(&self, x: i32, y: i32, z: i32) -> StateId;
    fn set_id(&mut self, x: i32, y: i32, z: i32, state: StateId);
    fn base_facts(&self, x: i32, y: i32, z: i32) -> BaseStateFacts;
}

