//! Typed block-state helpers shared by mob terrain consumers.

use lodestone_data::block_states;
use lodestone_entity::pathfinding::PathType;
use lodestone_model::PathType as CensusPathType;

/// Translates `lodestone_model::PathType` (what the census in
/// [`lodestone_data::path_types`] is keyed by) into
/// `lodestone_entity::pathfinding::PathType` (what the A* search and malus
/// table consume). The two enums are deliberately separate crates on
/// opposite sides of the version seam — see `pathfinding/mod.rs`'s own doc
/// ("a real adapter... maps real block-state ids to `PathType`") — so this is
/// the translation layer that doc promises, not a rename. Every variant is
/// named on both sides identically; the match is exhaustive on the census
/// side so a future variant added to either enum fails to compile here
/// instead of silently falling through.
pub(super) fn census_to_pathfinding_type(pt: CensusPathType) -> PathType {
    match pt {
        CensusPathType::Blocked => PathType::Blocked,
        CensusPathType::Open => PathType::Open,
        CensusPathType::Walkable => PathType::Walkable,
        CensusPathType::WalkableDoor => PathType::WalkableDoor,
        CensusPathType::Trapdoor => PathType::Trapdoor,
        CensusPathType::PowderSnow => PathType::PowderSnow,
        CensusPathType::OnTopOfPowderSnow => PathType::OnTopOfPowderSnow,
        CensusPathType::Fence => PathType::Fence,
        CensusPathType::Lava => PathType::Lava,
        CensusPathType::Water => PathType::Water,
        CensusPathType::WaterBorder => PathType::WaterBorder,
        CensusPathType::Rail => PathType::Rail,
        CensusPathType::UnpassableRail => PathType::UnpassableRail,
        CensusPathType::FireInNeighbor => PathType::FireInNeighbor,
        CensusPathType::Fire => PathType::Fire,
        CensusPathType::DamagingInNeighbor => PathType::DamagingInNeighbor,
        CensusPathType::Damaging => PathType::Damaging,
        CensusPathType::DoorOpen => PathType::DoorOpen,
        CensusPathType::DoorWoodClosed => PathType::DoorWoodClosed,
        CensusPathType::DoorIronClosed => PathType::DoorIronClosed,
        CensusPathType::Breach => PathType::Breach,
        CensusPathType::Leaves => PathType::Leaves,
        CensusPathType::StickyHoney => PathType::StickyHoney,
        CensusPathType::Cocoa => PathType::Cocoa,
        CensusPathType::DamageCautious => PathType::DamageCautious,
        CensusPathType::OnTopOfTrapdoor => PathType::OnTopOfTrapdoor,
        CensusPathType::BigMobsCloseToDanger => PathType::BigMobsCloseToDanger,
    }
}

pub(crate) fn fluid_state_id(
    state: block_states::StateId,
) -> Option<crate::fluid::FluidState> {
    crate::fluid::fluid_state_of_id(state)
}
