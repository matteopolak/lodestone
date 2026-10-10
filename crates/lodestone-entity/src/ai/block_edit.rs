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
    /// Whether the host reports back whether the edit landed
    /// ([`MobController::take_edit_result`](super::mob::MobController::take_edit_result)).
    pub ack: bool,
    /// Whether the host also requires the cell to be a legal place to put the
    /// state: a full solid block that is not bedrock below it, the state able to
    /// stand there, and no creature in the cell.
    pub checked_placement: bool,
    /// The mob that asked, stamped by the host when it drains the edit.
    pub requester: Option<i32>,
}

impl BlockEdit {
    /// An edit with no acknowledgement and no placement check.
    #[must_use]
    pub fn new(cell: (i32, i32, i32), expect: BlockExpect, set: Option<StateId>) -> Self {
        Self { cell, expect, set, ack: false, checked_placement: false, requester: None }
    }

    /// Asks the host to report whether the edit landed.
    #[must_use]
    pub fn acknowledged(mut self) -> Self {
        self.ack = true;
        self
    }

    /// Asks the host to validate the cell as a placement site.
    #[must_use]
    pub fn checked_placement(mut self) -> Self {
        self.checked_placement = true;
        self
    }
}
