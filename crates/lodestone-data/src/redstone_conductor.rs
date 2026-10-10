//! Whether a block state conducts redstone power, per the real game.
//!
//! # What it is
//!
//! One bit per global block-state id, taken from a headless server walk of the
//! `mc-version` release (`just oracle-redstone-conductor`). A bat roosts only
//! under a conducting block, so a slab, glass, leaves or an extended piston do
//! not count.
//!
//! # How to change it
//!
//! The table is generated: `just oracle-redstone-conductor` then
//! `just regen-redstone-conductor`. Do not edit `generated/redstone_conductor.rs`.

use crate::{block_states::StateId, generated_redstone_conductor as table};

pub use table::STATE_COUNT;

/// Whether the state conducts redstone power.
#[must_use]
pub fn conducts(state: StateId) -> bool {
    let id = state.raw() as usize;
    table::REDSTONE_CONDUCTOR[id / 64] >> (id % 64) & 1 != 0
}
