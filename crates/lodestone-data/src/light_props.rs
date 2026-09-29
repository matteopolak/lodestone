//! Exact scalar light inputs for the canonical 26.2 state palette.
//!
//! Raw dampening and emission are captured for all 32,366 states by querying
//! the initialized server registry. The capture's IDs, names and complete
//! property sets agree with the official 26.2 report and canonical state table.
//! See `docs/block-light-inputs.md` for provenance and regeneration.
//!
//! Dampening is `0..=15` before ordinary propagation applies its minimum cost
//! of one; emission is `0..=15`. Both are per state: lit furnaces, berry-bearing
//! vines, light levels, and waterlogged slabs cannot share a block default.
//! These scalar inputs do not encode adjacent face occlusion. A solver needs
//! both states' opposing faces and the direction to resolve that separate rule.
//!
//! The table stores distinct pairs plus one `u8` index per state, so lookup is
//! O(1) and allocates nothing. `tests/light_props.rs` owns exact generation,
//! structural checks, and bounded independent captured witnesses.
//!
//! The integrated server consumes these values through its
//! `lodestone_world::LightProperties` implementation.

use crate::generated_light_props as table;
use crate::block_states::StateId;

pub use table::STATE_COUNT;

/// The `(dampening, emission)` pair for a validated block-state id.
///
/// `StateId` proves the index is in the complete generated table, so this
/// lookup is total. Zero-heap: reads straight from rodata. O(1) indexing, no
/// search.
#[must_use]
pub fn light_props(id: StateId) -> (u8, u8) {
    let entry = table::STATE_ENTRY[id.raw() as usize];
    table::ENTRIES[entry as usize]
}

/// The raw light dampening for a validated block-state id.
///
/// This is the value before the engine applies its own `max(1, ·)` floor.
#[must_use]
pub fn dampening(id: StateId) -> u8 {
    light_props(id).0
}

/// The light emission for a validated block-state id.
#[must_use]
pub fn emission(id: StateId) -> u8 {
    light_props(id).1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_states::StateId;

    #[test]
    fn state_id_boundary_rejects_invalid_raw_ids_and_lookup_is_total() {
        assert!(StateId::new(STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());

        let valid = StateId::new(0).expect("state zero is in the generated table");
        let (dampening, emission) = light_props(valid);
        assert!(dampening <= 15);
        assert!(emission <= 15);
        assert_eq!(dampening, super::dampening(valid));
        assert_eq!(emission, super::emission(valid));
    }
}
