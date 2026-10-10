//! A world change a goal asks the host to make.
//!
//! The mob seam holds the world read-only, so a goal that places or removes a
//! block records a [`BlockEdit`] and the host applies it after the mob loop,
//! only if the cell still holds what the goal saw.

use lodestone_data::block_states::StateId;

/// What a cell must hold for a [`BlockEdit`] to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockExpect {
    /// Any air block.
    Air,
    /// Exactly this state.
    State(StateId),
    /// Any state of this block, whatever its properties.
    Block(lodestone_data::block::Block),
}

/// One requested block change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEdit {
    /// The cell to change.
    pub cell: (i32, i32, i32),
    /// What the cell must hold for the edit to apply.
    pub expect: BlockExpect,
    /// The state to write; `None` removes the block.
    pub set: Option<StateId>,
}
