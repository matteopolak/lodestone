//! The current tick task's explicit ownership of its chunk work.
//!
//! Regionised ticking eventually needs a non-overlapping owner for every chunk
//! it advances. Today there is one tick task, so this module intentionally has
//! one owner: [`TickOwner::Global`]. It nevertheless keeps the ownership shape
//! on the production path, rather than leaving a raw chunk list for a future
//! threaded implementation to reinterpret. It also exposes a read-only spatial
//! workload report so a named scene can be evaluated before choosing workers.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

/// The task that may advance a chunk during the current tick.
///
/// This is deliberately not a region coordinate yet. Selecting a region size
/// or adding concurrent owners requires the parity and populated-world
/// profiling work documented in `docs/plans/regionised-server-ticking.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOwner {
    /// The single server tick task owns every selected chunk.
    Global,
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
    owner: TickOwner,
    chunks: Vec<(i32, i32)>,
    workloads: [TickOwnerWorkload; 1],
}

impl TickRegionPlan {
    /// Makes the current single-owner plan from a duplicate-free chunk set.
    ///
    /// # Panics
    ///
    /// Panics when `chunks` contains a duplicate. A duplicate would otherwise
    /// give the one global owner two advances of the same chunk.
    #[must_use]
    pub fn global(chunks: Vec<(i32, i32)>) -> Self {
        assert!(
            chunks
                .iter()
                .enumerate()
                .all(|(index, chunk)| !chunks[..index].contains(chunk)),
            "tick-region chunks must be unique before ownership is assigned"
        );
        let chunks_len = chunks.len();
        Self {
            owner: TickOwner::Global,
            chunks,
            workloads: [TickOwnerWorkload {
                owner: TickOwner::Global,
                chunks: chunks_len,
            }],
        }
    }

    /// The owner of every chunk in this plan.
    #[must_use]
    pub fn owner(&self) -> TickOwner {
        self.owner
    }

    /// The chunks this owner advances, in deterministic visit order.
    #[must_use]
    pub fn chunks(&self) -> &[(i32, i32)] {
        &self.chunks
    }

    /// The current tick-owner workload. The production path consumes this
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
        let plan = TickRegionPlan::global(vec![(-3, 4), (0, 0), (7, -2)]);

        assert_eq!(plan.owner(), TickOwner::Global);
        assert_eq!(plan.chunks(), [(-3, 4), (0, 0), (7, -2)]);
        assert_eq!(
            plan.owner_workloads(),
            [TickOwnerWorkload {
                owner: TickOwner::Global,
                chunks: 3,
            }]
        );
    }

    #[test]
    fn a_candidate_grid_reports_negative_and_boundary_chunks_without_claiming_owners() {
        let plan = TickRegionPlan::global(vec![
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
        assert_eq!(plan.owner_workloads()[0].owner, TickOwner::Global);
    }

    #[test]
    #[should_panic(expected = "must be unique")]
    fn duplicate_chunks_are_rejected_before_they_can_receive_two_advances() {
        let _ = TickRegionPlan::global(vec![(0, 0), (0, 0)]);
    }
}
