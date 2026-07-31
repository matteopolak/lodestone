//! Block navigation path types for protocol 776 (Minecraft 26.2).
//!
//! Land navigation classifies each block state into a [`PathType`] — open,
//! blocked, water, lava, fence, closed door, rail, damaging, … — via vanilla's
//! `WalkNodeEvaluator.getPathTypeFromState`. This is the version-free seam
//! ([`lodestone_model::PathTypeRegistry`]); the table itself is 26.2 game data
//! generated from a headless-server dump and lives here in this data crate
//! (issue #361) rather than in `lodestone-v770` — which is exactly what lets
//! `lodestone-server` (zero protocol dependency by design) read it directly
//! for real node classification instead of the solid/air approximation
//! `ChunkWorld` uses today (issue #204).
//!
//! # Data source: interrogate the real jar, not `minecraft-data`
//!
//! The classification is *not* derivable from `blocks.json`: it depends on block
//! tags (`FENCES`, `WALLS`, `TRAPDOORS`, `SPELEOTHEMS`), fluid tags
//! (`WATER`/`LAVA`), `instanceof` (`DoorBlock`, `BaseRailBlock`,
//! `LeavesBlock`, `FenceGateBlock`) and `isPathfindable`. So, exactly like the
//! collision-shape table, the only authoritative source is the game itself.
//!
//! The table is generated from a dump produced by
//! `oracle-java/PathTypeOracle.java`, which boots the real 26.2 server, **loads
//! the vanilla data pack tags** (a `Bootstrap` alone leaves tag sets empty,
//! which silently mis-classifies fences/walls/trapdoors/water/lava), then calls
//! `getPathTypeFromState` for every one of the 32,366 states. "Boot the jar and
//! ask it" is the preferred data source over stale community datasets.
//!
//! Only the *base* per-state path types are produced here. A pathfinder adds
//! the neighbour-context variants ([`PathType::WaterBorder`],
//! [`PathType::FireInNeighbor`], …) itself; they exist in [`PathType`] so both
//! sides share one vocabulary.
//!
//! # Memory design
//!
//! Pure rodata, zero heap, O(1) by id: `STATE_PATH_TYPE: [PathType; 32_366]`.
//! [`PathType`] is a fieldless enum, so each entry is a single byte — the whole
//! table is ~32 KiB of rodata with no pointers to chase.

use lodestone_model::{PathType, PathTypeRegistry};

use crate::generated_path_types as table;

pub use table::STATE_COUNT;

/// The base navigation [`PathType`] for block-state `id`, or `None` if `id` is
/// not in `0..`[`STATE_COUNT`].
///
/// Zero-heap: a direct index into rodata. O(1), no search.
#[must_use]
pub fn path_type(id: u32) -> Option<PathType> {
    table::STATE_PATH_TYPE.get(id as usize).copied()
}

/// Zero-sized [`PathTypeRegistry`] over the generated protocol-776 table.
///
/// ```
/// use lodestone_model::{PathType, PathTypeRegistry};
/// use lodestone_data::path_types::PathTypes;
///
/// let reg = PathTypes;
/// assert_eq!(reg.path_type(0), Some(PathType::Open)); // air
/// assert_eq!(reg.path_type(1), Some(PathType::Blocked)); // stone
/// assert_eq!(reg.state_count(), lodestone_data::path_types::STATE_COUNT);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct PathTypes;

impl PathTypeRegistry for PathTypes {
    fn path_type(&self, id: u32) -> Option<PathType> {
        path_type(id)
    }

    fn state_count(&self) -> u32 {
        STATE_COUNT
    }
}
