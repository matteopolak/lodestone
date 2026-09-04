//! Exact inverse of the pre-1.13 composite-to-canonical state mapping.
//!
//! The legacy wire value is not a canonical state id: it is `(old_id << 4) |
//! meta`, and several legacy pairs can resolve to the same canonical state.
//! This module builds the inverse image once from [`crate::canonical::resolve`]
//! and keeps the smallest packed legacy representative for each reachable
//! canonical state. States outside that exact image are rejected explicitly;
//! this layer never substitutes air or invents a legacy value.

use std::sync::OnceLock;

use lodestone_data::block_states;

use crate::canonical::{self, CanonicalBlockState};

/// A state id that cannot be represented by an exact pre-1.13 pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InverseError {
    /// No legacy `(old_id, meta)` pair resolves to this canonical state.
    Unsupported { state: u32 },
}

/// Resolves a canonical 26.2 state to the minimum packed pre-1.13
/// representative in the exact image of [`canonical::resolve`].
///
/// The result uses the wire/storage algebra `(old_id << 4) | meta`. A state
/// that is valid in the canonical registry can still be unsupported here when
/// no legacy pair maps to it.
#[must_use]
pub fn resolve(state: u32) -> Result<u32, InverseError> {
    inverse_table()
        .get(state as usize)
        .copied()
        .flatten()
        .ok_or(InverseError::Unsupported { state })
}

fn inverse_table() -> &'static [Option<u32>] {
    static TABLE: OnceLock<Box<[Option<u32>]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = vec![None; block_states::STATE_COUNT as usize];
        for old_id in 0..=255u8 {
            for meta in 0..16u8 {
                let CanonicalBlockState::Resolved(state) = canonical::resolve(old_id, meta) else {
                    continue;
                };
                let Some(entry) = table.get_mut(state as usize) else {
                    continue;
                };
                let packed = (u32::from(old_id) << 4) | u32::from(meta);
                if entry.is_none_or(|current| packed < current) {
                    *entry = Some(packed);
                }
            }
        }
        table.into_boxed_slice()
    })
}
