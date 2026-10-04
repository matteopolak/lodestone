//! Bounded, resident-only terrain observations for native server plugins.
//!
//! The tick-owned ECS world must not expose a chunk-source guard or a callback
//! that can re-enter the source while a plugin system is running. This module
//! therefore keeps one shared source handle and returns copied state ids only.
//! A missing or contended column is reported as `None`; a read never starts
//! generation or waits for a source lock.

use std::sync::Arc;

use bevy_ecs::resource::Resource;
use lodestone_data::block_states::StateId;
use lodestone_model::BlockPos;

use crate::chunk::ChunkSource;
use crate::chunk_store::TryResident;

/// Maximum number of cells one plugin system may sample in one call.
pub const MAX_WORLD_SNAPSHOT_POSITIONS: usize = 128;

/// Why a native plugin's bounded world snapshot could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldSnapshotError {
    /// The request would exceed [`MAX_WORLD_SNAPSHOT_POSITIONS`].
    TooManyPositions { requested: usize, limit: usize },
}

/// A native-plugin read view over the authoritative primary terrain source.
///
/// The resource stores only an `Arc` to the source; each method copies its
/// answers before returning. Sources with the resident-only capability use an
/// atomic try boundary. Older sources may provide `resident_block_state_id` as
/// a non-blocking fallback, but a source is never asked to call `block_state`.
#[derive(Resource, Clone)]
pub struct ServerWorldSnapshot {
    source: Arc<dyn ChunkSource>,
}

impl std::fmt::Debug for ServerWorldSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerWorldSnapshot")
            .finish_non_exhaustive()
    }
}

impl ServerWorldSnapshot {
    /// Creates a view over the source that serves the connected clients.
    #[must_use]
    pub fn new(source: Arc<dyn ChunkSource>) -> Self {
        Self { source }
    }

    /// Reads one resident cell without entering generation.
    #[must_use]
    pub fn read_block(&self, position: BlockPos) -> Option<StateId> {
        match self
            .source
            .try_resident_block_state_id(position.x, position.y, position.z)
        {
            Some(TryResident::Present(state)) => Some(state),
            Some(TryResident::Busy | TryResident::Absent) => None,
            None => self
                .source
                .resident_block_state_id(position.x, position.y, position.z),
        }
    }
}
