//! Deterministic ownership for one synchronous chunk lifecycle operation.
//!
//! Ticket transitions and LRU eviction both eventually release a column through
//! [`crate::chunk::ChunkSource::unload`]. This module makes the owner of that
//! release explicit before a future region executor can move it off the
//! current task. It deliberately starts no worker: the caller executes every
//! assignment serially in canonical chunk-coordinate order.

/// The smallest owner that can load or unload one chunk column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkLifecycleOwner {
    /// The column at `(cx, cz)`, including negative coordinates unchanged.
    Chunk { cx: i32, cz: i32 },
}

/// One load or unload action and the chunk owner responsible for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChunkLifecycleAssignment {
    /// The sole owner of this lifecycle action.
    pub owner: ChunkLifecycleOwner,
    /// The target chunk coordinate.
    pub chunk: (i32, i32),
}

/// A bounded, duplicate-free lifecycle hand-off for columns already selected
/// by the current cache operation.
///
/// The plan owns no columns and has no queue. Its size is bounded by the
/// caller's selected inputs: an on-demand load has one assignment, while an
/// eviction batch contains at most the cache's current resident entries. The
/// order is `(cx, cz)`, so a `HashMap`-derived ticket delta cannot change the
/// serial source-unload order from one process run to another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkLifecyclePlan {
    assignments: Vec<ChunkLifecycleAssignment>,
}

impl ChunkLifecyclePlan {
    /// Plans an on-demand load without changing demand-driven generation.
    #[must_use]
    pub(crate) fn load(chunk: (i32, i32)) -> Self {
        Self::from_chunks([chunk])
    }

    /// Plans a cache-release batch after ticket or LRU selection.
    #[must_use]
    pub(crate) fn unload(chunks: impl IntoIterator<Item = (i32, i32)>) -> Self {
        Self::from_chunks(chunks)
    }

    fn from_chunks(chunks: impl IntoIterator<Item = (i32, i32)>) -> Self {
        let mut chunks: Vec<_> = chunks.into_iter().collect();
        chunks.sort_unstable();
        chunks.dedup();
        Self {
            assignments: chunks
                .into_iter()
                .map(|(cx, cz)| ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx, cz },
                    chunk: (cx, cz),
                })
                .collect(),
        }
    }

    /// The serial, canonical owner hand-off the current task consumes.
    #[must_use]
    pub(crate) fn assignments(&self) -> &[ChunkLifecycleAssignment] {
        &self.assignments
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_assignments_canonically_order_negative_chunks_and_deduplicate() {
        let plan = ChunkLifecyclePlan::unload([(0, 0), (-1, 2), (0, -1), (-1, 2), (-2, 0)]);

        assert_eq!(
            plan.assignments(),
            [
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: -2, cz: 0 },
                    chunk: (-2, 0),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: -1, cz: 2 },
                    chunk: (-1, 2),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: 0, cz: -1 },
                    chunk: (0, -1),
                },
                ChunkLifecycleAssignment {
                    owner: ChunkLifecycleOwner::Chunk { cx: 0, cz: 0 },
                    chunk: (0, 0),
                },
            ]
        );
    }

    #[test]
    fn an_on_demand_load_has_exactly_its_target_chunk_owner() {
        let plan = ChunkLifecyclePlan::load((-1, -1));

        assert_eq!(
            plan.assignments(),
            [ChunkLifecycleAssignment {
                owner: ChunkLifecycleOwner::Chunk { cx: -1, cz: -1 },
                chunk: (-1, -1),
            }]
        );
    }
}
