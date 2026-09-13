//! Exact per-state facts used by simple-block survival during world generation.
//!
//! The four predicates are compact bitsets keyed by [`StateId`]'s authoritative
//! global state id. They come from a compiled-reference registry walk using an
//! empty block getter at the origin; this is the same no-neighbour convention
//! used by the collision and snow-support censuses.
//!
//! `fire_flammable` is the fire block's exact `canBurn` answer. The complete
//! ignite and burn odds remain available through [`crate::block_blast`] for
//! consumers that need probabilities rather than this survival predicate.

use crate::block_states::StateId;
use crate::generated_block_survival as table;

pub use table::STATE_COUNT;

fn bit(bits: &[u8], id: StateId) -> bool {
    let raw = id.raw();
    bits[(raw / 8) as usize] & (1u8 << (raw % 8)) != 0
}

/// The state's exact solid-render flag.
#[must_use]
pub fn solid_render(id: StateId) -> bool {
    bit(&table::SOLID_RENDER, id)
}

/// Whether the state gives full support to its upward face.
#[must_use]
pub fn sturdy_up(id: StateId) -> bool {
    bit(&table::STURDY_UP, id)
}

/// Whether the state gives center support to its downward face.
#[must_use]
pub fn center_support_down(id: StateId) -> bool {
    bit(&table::CENTER_SUPPORT_DOWN, id)
}

/// Whether the fire block treats the exact state as flammable.
///
/// Waterlogged variants are false even when their block's default state is
/// flammable, because this table is state-indexed rather than block-indexed.
#[must_use]
pub fn fire_flammable(id: StateId) -> bool {
    bit(&table::FIRE_FLAMMABLE, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_boundary_and_known_controls() {
        assert_eq!(STATE_COUNT, crate::block_states::STATE_COUNT);
        assert!(StateId::new(STATE_COUNT).is_none());
        let state = |name| StateId::from_state_str(name).expect("known compiled state");
        assert!(solid_render(state("minecraft:stone")));
        assert!(sturdy_up(state("minecraft:stone")));
        assert!(center_support_down(state("minecraft:stone")));
        assert!(!solid_render(state("minecraft:water[level=0]")));
        assert!(!sturdy_up(state("minecraft:water[level=0]")));
        assert!(!center_support_down(state("minecraft:water[level=0]")));
        assert!(fire_flammable(state("minecraft:oak_planks")));
        assert!(!fire_flammable(state(
            "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=true,west=false]"
        )));
    }
}
