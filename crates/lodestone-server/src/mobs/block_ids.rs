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

/// The global block-state id for a canonical state string, or `None` for a
/// block name this version's census does not carry.
///
/// Delegates straight to [`lodestone_data::block_states::state_id`]. This used
/// to build and query its own 32,366-entry `HashMap<String, u32>` — one
/// `String` allocation per state at first use, one SipHash per lookup — added
/// for [`crate::block_drops`]'s correct-tool gate (issue #539). `lodestone-data`
/// now carries the forward index itself (a block-major sorted-span lookup,
/// `O(log 1196)` plus a scan bounded by that one block's own state count), so
/// building a second copy of the same map in this crate was exactly the
/// duplicated-inverse defect this function's own doc used to warn against.
///
/// Accepts a bare name (`"minecraft:stone"`) or one with properties
/// (`"minecraft:oak_log[axis=y]"`), since that is exactly what
/// [`ChunkColumn::block_state`] returns. Unlike the old exact-match map, a bare
/// or partial name now resolves through vanilla's
/// `defaultBlockState().setValue(…)` semantics (see [`block_states::state_id`]'s
/// doc for the three-tier fallback) instead of missing.
#[must_use]
pub(crate) fn block_state_id(name: &str) -> Option<u32> {
    block_states::state_id(name)
}

/// Was a second door with a lowest-id-by-block fallback for a bare or partial
/// name — [`block_states::state_id`] already resolves those to the
/// jar-marked default state (properties named by the caller overridden on top
/// of it), which is what this function's callers actually wanted, so the two
/// doors are now the same call. Kept as a separate name so call sites do not
/// need to change.
///
/// Do not reintroduce a lowest-id fallback here: `defaultBlockState()` is not
/// the lowest id, and [`block_states::state_id`]'s own doc names the three
/// shipped bugs that assumption caused.
#[must_use]
pub(crate) fn block_state_id_or_default(name: &str) -> Option<u32> {
    block_states::state_id(name)
}
