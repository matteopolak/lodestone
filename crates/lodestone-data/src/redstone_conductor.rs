//! Whether a block state conducts redstone power, per the real game.
//!
//! # What it is
//!
//! One bit per global block-state id, taken from a headless 26.2 server walk
//! (`just oracle-redstone-conductor`). A bat roosts only under a conducting
//! block, so a slab, glass, leaves or an extended piston do not count.
//!
//! Only the 26.2 state-id prefix is captured; the block-state table is an
//! append-only union with later releases, so a state added after 26.2 is
//! unknown here and the caller picks a fallback.
//!
//! # How to change it
//!
//! The table is generated: `just oracle-redstone-conductor` then
//! `just regen-redstone-conductor`. Do not edit `generated/redstone_conductor.rs`.

use crate::{block_states::StateId, generated_redstone_conductor as table};

pub use table::STATE_COUNT;

/// Whether the state conducts redstone power, or `None` for a state added
/// after 26.2.
#[must_use]
pub fn conducts(state: StateId) -> Option<bool> {
    let id = state.raw() as usize;
    if state.raw() >= STATE_COUNT {
        return None;
    }
    Some(table::REDSTONE_CONDUCTOR[id / 64] >> (id % 64) & 1 != 0)
}
