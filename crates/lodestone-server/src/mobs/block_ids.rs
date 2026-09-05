//! Block-state id lookups shared by [`super::ChunkWorld`] and by the wider
//! crate through the [`block_state_id`]/[`block_state_id_or_default`] doors —
//! moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs` file split
//! (see `docs/plans/crate-and-file-splits.md`). No `MobSim` dependency: this
//! is the pure block-state-id half of the terrain adapter.

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

/// The validated global block-state id for a canonical state string, or `None`
/// for a block name this version's census does not carry.
///
/// Delegates straight to [`block_states::StateId::from_state_str`]. The
/// generated table validates its range at this string boundary, so every
/// downstream census lookup is total. A name owned by a plug-in or data pack is
/// deliberately still `None`: this server does not invent a built-in state for
/// it.
///
/// Accepts a bare name (`"minecraft:stone"`) or one with properties
/// (`"minecraft:oak_log[axis=y]"`), since that is exactly what
/// [`ChunkColumn::block_state`] returns. Unlike the old exact-match map, a bare
/// or partial name now resolves through vanilla's
/// `defaultBlockState().setValue(…)` semantics (see
/// [`block_states::StateId::from_state_str`]'s doc) instead of missing.
#[must_use]
pub(crate) fn block_state_id(name: &str) -> Option<block_states::StateId> {
    block_states::StateId::from_state_str(name)
}

/// Resolves a bare or partial name to the jar-marked default state with the
/// caller's named properties overridden on top. Kept as a separate name to
/// make consumers that intentionally accept partial input explicit.
///
/// Do not replace this with a lowest-id fallback: a block's registered default
/// state is not necessarily its lowest state id.
#[must_use]
pub(crate) fn block_state_id_or_default(name: &str) -> Option<block_states::StateId> {
    block_states::StateId::from_state_str(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_state_string_boundary_returns_only_generated_ids() {
        let resolve: fn(&str) -> Option<block_states::StateId> = block_state_id;
        let air = resolve("minecraft:air").expect("the generated air state resolves");
        assert_eq!(air.raw(), 0, "air's generated global state id is the fixed literal 0");

        assert_eq!(resolve("lodestone:custom_block"), None);
        assert_eq!(resolve("minecraft:not_a_real_block"), None);

        assert!(block_states::StateId::new(block_states::STATE_COUNT).is_none());
        assert!(block_states::StateId::new(u32::MAX).is_none());
    }
}
