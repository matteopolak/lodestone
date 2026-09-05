//! The current tick task's explicit ownership of its chunk work.
//!
//! Regionised ticking eventually needs a non-overlapping owner for every chunk
//! it advances. This module starts with the smallest possible region: one chunk
//! column. The current server executes those owners serially in its established
//! visit order; it does not claim that they can run concurrently. Keeping the
//! per-chunk assignment on the production path makes that later decision an
//! explicit change rather than an interpretation of a raw coordinate list.
//! It also exposes a read-only spatial workload report so a named scene can be
//! evaluated before choosing larger workers.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

/// The logical owner of a selected chunk during the current tick.
///
/// A [`Self::Chunk`] owner is a deliberately minimal region. The live tick loop
/// still executes all such owners serially in canonical visit order. Choosing a
/// larger ownership cell or adding concurrent owners requires the parity and
/// populated-world profiling work documented in
/// `docs/plans/regionised-server-ticking.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOwner {
    /// State that cannot yet be partitioned by chunk, such as world time and
    /// scheduled queues.
    Global,
    /// The smallest region-local owner: one chunk column at `(cx, cz)`.
    Chunk { cx: i32, cz: i32 },
}

/// One chunk assigned to its logical [`TickOwner`].
///
/// The sequence of these assignments is the canonical simulation visit order.
/// A future concurrent executor must not reorder it without a separately
/// validated deterministic hand-off design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOwnedChunk {
    /// The region that owns `chunk` for chunk-local tick work.
    pub owner: TickOwner,
    /// The owned chunk coordinate.
    pub chunk: (i32, i32),
}

/// A duplicate-free set of chunks belonging to one [`TickOwner`].
///
/// The ordering is part of simulation behaviour: random-tick draws consume the
/// chunks in visit order. The plan preserves the producer's existing stable
/// order while making duplicate ownership impossible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickOwnerWorkload {
    /// The task that advances these chunks.
    pub owner: TickOwner,
    /// The number of unique chunks that task advances in this tick.
    pub chunks: usize,
}

/// The selected work contained by one hypothetical spatial region.
///
/// This is a measurement cell, not a tick owner. In particular, constructing
/// it neither starts a worker nor selects a future region size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateRegionLoad {
    /// The cell coordinate, using Euclidean division so negative chunks remain
    /// on the negative side of an origin boundary.
    pub region: (i32, i32),
    /// The number of selected chunks inside this cell.
    pub chunks: usize,
}

/// A deterministic spatial workload report for one candidate region edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRegionWorkload {
    edge_chunks: NonZeroU32,
    total_chunks: usize,
    regions: Vec<CandidateRegionLoad>,
}

impl CandidateRegionWorkload {
    /// The candidate region edge used to group chunks.
    #[must_use]
    pub fn edge_chunks(&self) -> NonZeroU32 {
        self.edge_chunks
    }

    /// The number of selected chunks represented by this report.
    #[must_use]
    pub fn total_chunks(&self) -> usize {
        self.total_chunks
    }

    /// Non-empty candidate cells in stable coordinate order.
    #[must_use]
    pub fn regions(&self) -> &[CandidateRegionLoad] {
        &self.regions
    }

    /// The largest candidate-cell workload, or zero for an empty plan.
    #[must_use]
    pub fn largest_region_chunks(&self) -> usize {
        self.regions.iter().map(|load| load.chunks).max().unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct TickRegionPlan {
    chunks: Vec<(i32, i32)>,
    owned_chunks: Vec<TickOwnedChunk>,
    workloads: Vec<TickOwnerWorkload>,
}

impl TickRegionPlan {
    /// Assigns the current tick's duplicate-free chunk set to minimal regions.
    ///
    /// # Panics
    ///
    /// Panics when `chunks` contains a duplicate. A duplicate would otherwise
    /// give one chunk two advances in the same tick.
    #[must_use]
    pub fn chunk_owned(chunks: Vec<(i32, i32)>) -> Self {
        assert!(
            chunks
                .iter()
                .enumerate()
                .all(|(index, chunk)| !chunks[..index].contains(chunk)),
            "tick-region chunks must be unique before ownership is assigned"
        );
        let owned_chunks: Vec<_> = chunks
            .iter()
            .map(|&(cx, cz)| TickOwnedChunk {
                owner: TickOwner::Chunk { cx, cz },
                chunk: (cx, cz),
            })
            .collect();
        let workloads = owned_chunks
            .iter()
            .map(|owned| TickOwnerWorkload {
                owner: owned.owner,
                chunks: 1,
            })
            .collect();
        Self {
            chunks,
            owned_chunks,
            workloads,
        }
    }

    /// The selected chunks in deterministic visit order.
    #[must_use]
    pub fn chunks(&self) -> &[(i32, i32)] {
        &self.chunks
    }

    /// The selected chunks with their region-local owner, in exactly the same
    /// deterministic visit order as [`Self::chunks`].
    #[must_use]
    pub fn owned_chunks(&self) -> &[TickOwnedChunk] {
        &self.owned_chunks
    }

    /// The current region-owner workloads. The production path consumes this
    /// report when it derives the simulated-chunk count.
    #[must_use]
    pub fn owner_workloads(&self) -> &[TickOwnerWorkload] {
        &self.workloads
    }

    /// Groups selected chunks into read-only candidate spatial regions.
    ///
    /// `edge_chunks` is supplied by the observer so this seam does not turn a
    /// profiling hypothesis into a server configuration choice. The report has
    /// no effect on ownership, visit order, or the tick loop.
    #[must_use]
    pub fn candidate_region_workload(
        &self,
        edge_chunks: NonZeroU32,
    ) -> CandidateRegionWorkload {
        let edge = i64::from(edge_chunks.get());
        let mut counts = BTreeMap::new();
        for &(cx, cz) in &self.chunks {
            let region = (
                i32::try_from(i64::from(cx).div_euclid(edge))
                    .expect("candidate region x must fit in i32"),
                i32::try_from(i64::from(cz).div_euclid(edge))
                    .expect("candidate region z must fit in i32"),
            );
            *counts.entry(region).or_insert(0usize) += 1;
        }
        CandidateRegionWorkload {
            edge_chunks,
            total_chunks: self.chunks.len(),
            regions: counts
                .into_iter()
                .map(|(region, chunks)| CandidateRegionLoad { region, chunks })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_single_tick_task_owns_each_canonical_chunk_once() {
        let plan = TickRegionPlan::chunk_owned(vec![(-3, 4), (0, 0), (7, -2)]);

        assert_eq!(plan.chunks(), [(-3, 4), (0, 0), (7, -2)]);
        assert_eq!(
            plan.owned_chunks(),
            [
                TickOwnedChunk {
                    owner: TickOwner::Chunk { cx: -3, cz: 4 },
                    chunk: (-3, 4),
                },
                TickOwnedChunk {
                    owner: TickOwner::Chunk { cx: 0, cz: 0 },
                    chunk: (0, 0),
                },
                TickOwnedChunk {
                    owner: TickOwner::Chunk { cx: 7, cz: -2 },
                    chunk: (7, -2),
                },
            ]
        );
        assert_eq!(
            plan.owner_workloads(),
            [
                TickOwnerWorkload {
                    owner: TickOwner::Chunk { cx: -3, cz: 4 },
                    chunks: 1,
                },
                TickOwnerWorkload {
                    owner: TickOwner::Chunk { cx: 0, cz: 0 },
                    chunks: 1,
                },
                TickOwnerWorkload {
                    owner: TickOwner::Chunk { cx: 7, cz: -2 },
                    chunks: 1,
                },
            ]
        );
    }

    #[test]
    fn a_candidate_grid_reports_negative_and_boundary_chunks_without_claiming_owners() {
        let plan = TickRegionPlan::chunk_owned(vec![
            (-1, -1),
            (0, 0),
            (7, 7),
            (8, 8),
            (15, 8),
            (16, 8),
        ]);

        let workload = plan.candidate_region_workload(NonZeroU32::new(8).unwrap());

        assert_eq!(workload.edge_chunks().get(), 8);
        assert_eq!(workload.total_chunks(), 6);
        assert_eq!(workload.largest_region_chunks(), 2);
        assert_eq!(
            workload.regions(),
            [
                CandidateRegionLoad {
                    region: (-1, -1),
                    chunks: 1,
                },
                CandidateRegionLoad {
                    region: (0, 0),
                    chunks: 2,
                },
                CandidateRegionLoad {
                    region: (1, 1),
                    chunks: 2,
                },
                CandidateRegionLoad {
                    region: (2, 1),
                    chunks: 1,
                },
            ]
        );
        assert_eq!(
            plan.owned_chunks()[0],
            TickOwnedChunk {
                owner: TickOwner::Chunk { cx: -1, cz: -1 },
                chunk: (-1, -1),
            }
        );
        assert_eq!(
            plan.owned_chunks()[5],
            TickOwnedChunk {
                owner: TickOwner::Chunk { cx: 16, cz: 8 },
                chunk: (16, 8),
            }
        );
    }

    #[test]
    #[should_panic(expected = "must be unique")]
    fn duplicate_chunks_are_rejected_before_they_can_receive_two_advances() {
        let _ = TickRegionPlan::chunk_owned(vec![(0, 0), (0, 0)]);
    }
}
