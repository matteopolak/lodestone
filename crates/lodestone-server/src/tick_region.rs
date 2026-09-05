//! The current tick task's explicit ownership of its chunk work.
//!
//! Regionised ticking eventually needs a non-overlapping owner for every chunk
//! it advances. Today there is one tick task, so this module intentionally has
//! one owner: [`TickOwner::Global`]. It nevertheless keeps the ownership shape
//! on the production path, rather than leaving a raw chunk list for a future
//! threaded implementation to reinterpret.

/// The task that may advance a chunk during the current tick.
///
/// This is deliberately not a region coordinate yet. Selecting a region size
/// or adding concurrent owners requires the parity and populated-world
/// profiling work documented in `docs/plans/regionised-server-ticking.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TickOwner {
    /// The single server tick task owns every selected chunk.
    Global,
}

/// A duplicate-free set of chunks belonging to one [`TickOwner`].
///
/// The ordering is part of simulation behaviour: random-tick draws consume the
/// chunks in visit order. The plan preserves the producer's existing stable
/// order while making duplicate ownership impossible.
#[derive(Debug)]
pub(crate) struct TickRegionPlan {
    owner: TickOwner,
    chunks: Vec<(i32, i32)>,
}

impl TickRegionPlan {
    /// Makes the current single-owner plan from a duplicate-free chunk set.
    ///
    /// # Panics
    ///
    /// Panics when `chunks` contains a duplicate. A duplicate would otherwise
    /// give the one global owner two advances of the same chunk.
    #[must_use]
    pub(crate) fn global(chunks: Vec<(i32, i32)>) -> Self {
        assert!(
            chunks
                .iter()
                .enumerate()
                .all(|(index, chunk)| !chunks[..index].contains(chunk)),
            "tick-region chunks must be unique before ownership is assigned"
        );
        Self {
            owner: TickOwner::Global,
            chunks,
        }
    }

    /// The owner of every chunk in this plan.
    #[must_use]
    pub(crate) fn owner(&self) -> TickOwner {
        self.owner
    }

    /// The chunks this owner advances, in deterministic visit order.
    #[must_use]
    pub(crate) fn chunks(&self) -> &[(i32, i32)] {
        &self.chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_single_tick_task_owns_each_canonical_chunk_once() {
        let plan = TickRegionPlan::global(vec![(-3, 4), (0, 0), (7, -2)]);

        assert_eq!(plan.owner(), TickOwner::Global);
        assert_eq!(plan.chunks(), [(-3, 4), (0, 0), (7, -2)]);
    }

    #[test]
    #[should_panic(expected = "must be unique")]
    fn duplicate_chunks_are_rejected_before_they_can_receive_two_advances() {
        let _ = TickRegionPlan::global(vec![(0, 0), (0, 0)]);
    }
}
