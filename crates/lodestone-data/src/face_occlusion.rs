//! Effective six-direction face occlusion for protocol 776 (Minecraft 26.2).
//!
//! This is the compact runtime seam for world-generation predicates that need
//! to know whether a neighboring state completely closes a covered face. It is
//! intentionally distinct from collision, outline, and motion-blocking facts:
//! those tables answer different questions for partial blocks, non-colliding
//! blocks, and blocks that opt out of visual occlusion.
//!
//! The generated table stores one six-bit mask per global block-state id. A
//! lookup validates its id once through [`StateId`], then reads one byte from
//! static rodata. Keep this seam in terms of `StateId`; a worldgen interner can
//! bind its local ids to canonical ids once per generation pass instead of
//! resolving a state string in the candidate loop.

use crate::{block_states::StateId, generated_face_occlusion as table};

pub use table::STATE_COUNT;

/// The six block faces, in the same bit order as the generated table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Face {
    /// -Y.
    Down = 0,
    /// +Y.
    Up = 1,
    /// -Z.
    North = 2,
    /// +Z.
    South = 3,
    /// -X.
    West = 4,
    /// +X.
    East = 5,
}

impl Face {
    /// All faces in generated-table bit order.
    pub const ALL: [Self; 6] = [
        Self::Down,
        Self::Up,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    /// The mask bit for this face.
    #[must_use]
    pub const fn mask(self) -> u8 {
        1u8 << (self as u8)
    }
}

/// Returns the six-direction occlusion mask for a validated state.
///
/// Bit `Face as u8` is set only when that face is a complete unit occluder.
/// This is one bounds-checked array lookup after `StateId` validation and does
/// not inspect or allocate a state string.
#[must_use]
pub fn occlusion_mask(state: StateId) -> u8 {
    table::FACE_OCCLUSION[state.raw() as usize]
}

/// Whether a validated state completely occludes `face`.
#[must_use]
pub fn occludes(state: StateId, face: Face) -> bool {
    occlusion_mask(state) & face.mask() != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_id_boundary_and_known_controls() {
        assert_eq!(STATE_COUNT, crate::block_states::STATE_COUNT);
        assert!(StateId::new(STATE_COUNT).is_none());
        assert!(StateId::new(u32::MAX).is_none());

        let air = StateId::from_state_str("minecraft:air").expect("air is in the state table");
        assert_eq!(occlusion_mask(air), 0, "air leaves every face visible");

        let stone =
            StateId::from_state_str("minecraft:stone").expect("stone is in the state table");
        assert_eq!(
            occlusion_mask(stone),
            0b00_111111,
            "stone closes every face"
        );
        for face in Face::ALL {
            assert!(occludes(stone, face));
        }
    }

    #[test]
    fn every_direction_has_both_values() {
        for face in Face::ALL {
            let set = (0..STATE_COUNT)
                .map(|raw| StateId::new(raw).expect("generated state id is valid"))
                .filter(|&state| occludes(state, face))
                .count();
            assert!(set > 0, "{face:?} is all-zero across {STATE_COUNT} states");
            assert!(
                set < STATE_COUNT as usize,
                "{face:?} is all-one across {STATE_COUNT} states"
            );
        }
    }
}
