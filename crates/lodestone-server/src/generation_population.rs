//! One shared claim per generated column, with bounded deferred population work.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU8, Ordering};

use lodestone_worldgen::spawn_stage::GenerationSpawn;

use crate::mob_spawn::SpawnCandidate;

const UNCLAIMED: u8 = 0;
const CLAIMED: u8 = 1;
const COMPLETED: u8 = 2;

/// Maximum number of column batches held outside their resident columns.
pub const PENDING_BATCH_LIMIT: usize = 256;
/// Maximum placement decisions per population cycle, including deferred probes.
pub const CANDIDATE_BUDGET: usize = 256;

/// Clone-shared identity and completion state for one generated column's candidates.
///
/// A copied terrain column retains this same object. Claiming a batch therefore
/// transfers its candidates once, while native persistence still sees pending
/// work until every candidate has a definitive placement decision.
#[derive(Debug)]
pub struct GenerationSpawnBatch {
    candidates: Mutex<Vec<GenerationSpawn>>,
    phase: AtomicU8,
    candidate_bytes: usize,
}

impl GenerationSpawnBatch {
    /// Empty columns carry no shared allocation.
    #[must_use]
    pub fn new(candidates: Vec<GenerationSpawn>) -> Option<Arc<Self>> {
        if candidates.is_empty() {
            return None;
        }
        Some(Arc::new(Self {
            candidate_bytes: candidates.capacity() * std::mem::size_of::<GenerationSpawn>(),
            candidates: Mutex::new(candidates),
            phase: AtomicU8::new(UNCLAIMED),
        }))
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.phase.load(Ordering::Acquire) != COMPLETED
    }

    #[must_use]
    pub fn is_claimable(&self) -> bool {
        self.phase.load(Ordering::Acquire) == UNCLAIMED
    }

    /// Conservative retained accounting includes a claimed candidate buffer.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + if self.is_pending() { self.candidate_bytes } else { 0 }
    }

    /// Explicit synchronous drain for storage fixtures and legacy owned snapshots.
    /// Production population uses the deferred claim held by `GenerationPopulation`.
    pub fn take_legacy(&self) -> Vec<GenerationSpawn> {
        let mut candidates = self.candidates.lock().expect("generation spawn batch lock poisoned");
        if self.phase.compare_exchange(UNCLAIMED, COMPLETED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return Vec::new();
        }
        std::mem::take(&mut *candidates)
    }

    fn claim(self: &Arc<Self>) -> Option<ClaimedBatch> {
        let mut candidates = self.candidates.try_lock().ok()?;
        self.phase.compare_exchange(UNCLAIMED, CLAIMED, Ordering::AcqRel, Ordering::Acquire).ok()?;
        Some(ClaimedBatch {
            source: Arc::clone(self),
            remaining: std::mem::take(&mut *candidates).into(),
        })
    }
}

/// Source-owned publication of unresolved batches whose terrain is retained.
#[derive(Debug, Default)]
pub struct PendingGenerationPopulationPublication {
    batches: Mutex<VecDeque<Arc<GenerationSpawnBatch>>>,
}

impl PendingGenerationPopulationPublication {
    /// Called by the generation worker after retaining the authoritative column.
    pub fn publish(&self, batch: &Arc<GenerationSpawnBatch>) {
        if !batch.is_pending() {
            return;
        }
        let mut batches = self.batches.lock().expect("generation population publication lock poisoned");
        batches.retain(|published| published.is_pending());
        if !batches.iter().any(|published| Arc::ptr_eq(published, batch)) {
            batches.push_back(Arc::clone(batch));
        }
    }

    /// Tick discovery never waits. Claimed entries remain until completion so
    /// dropping a consumer can make its unresolved candidates claimable again.
    #[must_use]
    pub fn pending(&self, limit: usize) -> Vec<Arc<GenerationSpawnBatch>> {
        let Ok(mut batches) = self.batches.try_lock() else {
            return Vec::new();
        };
        batches.retain(|batch| batch.is_pending());
        batches.iter().filter(|batch| batch.is_claimable()).take(limit).cloned().collect()
    }
}

#[derive(Debug)]
struct ClaimedBatch {
    source: Arc<GenerationSpawnBatch>,
    remaining: VecDeque<GenerationSpawn>,
}

impl Drop for ClaimedBatch {
    fn drop(&mut self) {
        if self.remaining.is_empty() {
            self.source.phase.store(COMPLETED, Ordering::Release);
        } else {
            let mut candidates = self.source.candidates.lock().expect("generation spawn batch lock poisoned");
            *candidates = self.remaining.drain(..).collect();
            self.source.phase.store(UNCLAIMED, Ordering::Release);
        }
    }
}

/// Unavailable terrain or light is distinct from a definitive placement failure.
#[derive(Debug)]
pub enum PlacementDecision {
    Deferred,
    Rejected,
    Accepted(SpawnCandidate),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PopulationProgress {
    pub spawned: usize,
    pub rejected: usize,
    pub deferred: usize,
    pub completed_batches: usize,
}

/// Per-world population consumer, shared by native and browser world ticks.
///
/// Holds only batches with unresolved candidates. Completion remains on the
/// source's clone-shared generation identity, so no historical coordinate set
/// grows as the player explores. Placement and materialization are separate
/// callbacks so expensive terrain/light work does not require the mob lock.
#[derive(Debug, Default)]
pub struct GenerationPopulation {
    pending: VecDeque<ClaimedBatch>,
}

impl GenerationPopulation {
    /// Sparse terrain coordinates still needed by admitted generation batches.
    pub fn pending_chunks(&self) -> impl Iterator<Item = (i32, i32)> + '_ {
        self.pending.iter().filter_map(|batch| {
            batch.remaining.front().map(|candidate| {
                (candidate.x.div_euclid(16), candidate.z.div_euclid(16))
            })
        })
    }

    #[must_use]
    pub fn remaining_batch_capacity(&self) -> usize {
        PENDING_BATCH_LIMIT.saturating_sub(self.pending.len())
    }

    /// Claims this generation once. A full queue leaves the source untouched.
    pub fn admit(&mut self, batch: &Arc<GenerationSpawnBatch>) -> bool {
        if self.remaining_batch_capacity() == 0 {
            return false;
        }
        let Some(claimed) = batch.claim() else {
            return false;
        };
        self.pending.push_back(claimed);
        true
    }

    /// Processes each admitted candidate at most once in this cycle.
    ///
    /// Successful materialization or definitive rejection removes a candidate;
    /// missing inputs and a failed materialization retain it. The final removal
    /// acknowledges this source batch. Generation population applies no natural
    /// category cap or player-distance exclusion.
    pub fn process(
        &mut self,
        mut classify: impl FnMut(&GenerationSpawn) -> PlacementDecision,
        mut materialize: impl FnMut(SpawnCandidate) -> bool,
    ) -> PopulationProgress {
        let mut progress = PopulationProgress::default();
        let mut budget = CANDIDATE_BUDGET;
        let batches = self.pending.len();
        for _ in 0..batches {
            let mut batch = self.pending.pop_front().expect("counted pending batch");
            let attempts = batch.remaining.len().min(budget);
            for _ in 0..attempts {
                let candidate = batch.remaining.front().expect("counted pending candidate");
                let decided = match classify(candidate) {
                    PlacementDecision::Deferred => {
                        progress.deferred += 1;
                        false
                    }
                    PlacementDecision::Rejected => {
                        progress.rejected += 1;
                        true
                    }
                    PlacementDecision::Accepted(accepted) => {
                        if materialize(accepted) {
                            progress.spawned += 1;
                            true
                        } else {
                            progress.deferred += 1;
                            false
                        }
                    }
                };
                let candidate = batch.remaining.pop_front().expect("classified pending candidate");
                if !decided {
                    batch.remaining.push_back(candidate);
                }
                budget -= 1;
            }
            if batch.remaining.is_empty() {
                progress.completed_batches += 1;
            } else {
                self.pending.push_back(batch);
            }
            if budget == 0 {
                break;
            }
        }
        progress
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_data::entity_type::EntityType;
    use lodestone_model::Vec3;

    fn batch(xs: &[i32]) -> Arc<GenerationSpawnBatch> {
        GenerationSpawnBatch::new(xs.iter().map(|&x| GenerationSpawn {
            entity_type: EntityType::Cow.into(),
            x,
            y: 64,
            z: 0,
        }).collect()).expect("nonempty batch")
    }

    fn accepted(raw: &GenerationSpawn) -> PlacementDecision {
        PlacementDecision::Accepted(SpawnCandidate {
            entity_type: "minecraft:cow".parse().expect("cow resource key"),
            pos: Vec3::new(f64::from(raw.x) + 0.5, 64.0, 0.5),
        })
    }

    #[test]
    fn publication_deduplicates_bounds_claimable_results_and_prunes_completion() {
        let publication = PendingGenerationPopulationPublication::default();
        let first = batch(&[3]);
        let second = batch(&[7]);
        let third = batch(&[11]);
        publication.publish(&first);
        publication.publish(&Arc::clone(&first));
        publication.publish(&second);
        publication.publish(&third);
        assert!(publication.pending(0).is_empty());
        let published = publication.pending(2);
        assert_eq!(published.len(), 2);
        assert!(Arc::ptr_eq(&published[0], &first));
        assert!(Arc::ptr_eq(&published[1], &second));
        assert_eq!(publication.batches.lock().expect("publication lock").len(), 3);

        let mut consumer = GenerationPopulation::default();
        assert!(consumer.admit(&first));
        let published = publication.pending(2);
        assert_eq!(published.len(), 2);
        assert!(Arc::ptr_eq(&published[0], &second));
        assert!(Arc::ptr_eq(&published[1], &third));
        assert_eq!(publication.batches.lock().expect("publication lock").len(), 3);
        assert_eq!(consumer.process(accepted, |_| true).spawned, 1);
        assert_eq!(publication.pending(usize::MAX).len(), 2);
        assert_eq!(publication.batches.lock().expect("publication lock").len(), 2);
        publication.publish(&first);
        assert_eq!(publication.pending(usize::MAX).len(), 2);
    }

    #[test]
    fn publication_retains_claim_for_consumer_drop_and_never_waits_on_busy_lock() {
        let publication = PendingGenerationPopulationPublication::default();
        let first = batch(&[3, 7]);
        publication.publish(&first);
        {
            let guard = publication.batches.lock().expect("publication lock");
            assert!(publication.pending(1).is_empty());
            drop(guard);
        }
        assert!(Arc::ptr_eq(&publication.pending(1)[0], &first));
        {
            let mut consumer = GenerationPopulation::default();
            assert!(consumer.admit(&first));
            consumer.process(
                |raw| if raw.x == 3 { accepted(raw) } else { PlacementDecision::Deferred },
                |_| true,
            );
            assert!(publication.pending(1).is_empty());
            assert_eq!(publication.batches.lock().expect("publication lock").len(), 1);
        }
        let published = publication.pending(1);
        assert_eq!(published.len(), 1);
        assert!(Arc::ptr_eq(&published[0], &first));
        let mut retry = GenerationPopulation::default();
        assert!(retry.admit(&published[0]));
        let mut positions = Vec::new();
        assert_eq!(retry.process(accepted, |spawn| { positions.push(spawn.pos.x); true }).spawned, 1);
        assert_eq!(positions, vec![7.5]);
        assert!(publication.pending(1).is_empty());
        assert!(publication.batches.lock().expect("publication lock").is_empty());
    }

    #[test]
    fn deferred_candidate_survives_and_shared_identity_materializes_once() {
        let batch = batch(&[3, 7]);
        let copy = Arc::clone(&batch);
        let mut consumer = GenerationPopulation::default();
        let mut competing = GenerationPopulation::default();
        assert!(consumer.admit(&batch));
        assert!(!consumer.admit(&copy));
        assert!(!competing.admit(&copy));
        let first = consumer.process(
            |raw| if raw.x == 3 { accepted(raw) } else { PlacementDecision::Deferred },
            |_| true,
        );
        assert_eq!(first, PopulationProgress { spawned: 1, deferred: 1, ..Default::default() });
        assert!(copy.is_pending());
        let mut positions = Vec::new();
        let second = consumer.process(accepted, |spawn| { positions.push(spawn.pos.x); true });
        assert_eq!(positions, vec![7.5]);
        assert_eq!(second.spawned, 1);
        assert_eq!(second.completed_batches, 1);
        assert!(!batch.is_pending());
        assert!(!competing.admit(&copy));
        assert_eq!(consumer.process(accepted, |_| panic!("completed candidate repeated")), PopulationProgress::default());
    }

    #[test]
    fn definitive_failure_acknowledges_and_same_coordinate_new_generation_is_independent() {
        let old = batch(&[3]);
        let regenerated = batch(&[3]);
        let mut consumer = GenerationPopulation::default();
        assert!(consumer.admit(&old));
        let progress = consumer.process(|_| PlacementDecision::Rejected, |_| panic!("rejected candidate materialized"));
        assert_eq!(progress.rejected, 1);
        assert!(!old.is_pending());
        assert!(consumer.admit(&regenerated));
        assert_eq!(consumer.process(accepted, |_| true).spawned, 1);
    }

    #[test]
    fn dropping_consumer_restores_only_unresolved_candidates() {
        let batch = batch(&[3, 7]);
        {
            let mut consumer = GenerationPopulation::default();
            assert!(consumer.admit(&batch));
            consumer.process(
                |raw| if raw.x == 3 { accepted(raw) } else { PlacementDecision::Deferred },
                |_| true,
            );
        }
        assert!(batch.is_pending());
        let mut retry = GenerationPopulation::default();
        assert!(retry.admit(&batch));
        let mut positions = Vec::new();
        retry.process(accepted, |spawn| { positions.push(spawn.pos.x); true });
        assert_eq!(positions, vec![7.5]);
        assert!(!batch.is_pending());
    }

    #[test]
    fn queue_bound_leaves_next_batch_unclaimed() {
        let mut consumer = GenerationPopulation::default();
        for _ in 0..PENDING_BATCH_LIMIT {
            assert!(consumer.admit(&batch(&[3])));
        }
        let overflow = batch(&[3]);
        assert!(!consumer.admit(&overflow));
        let mut independent = GenerationPopulation::default();
        assert!(independent.admit(&overflow));
    }

    #[test]
    fn failed_materialization_remains_pending_until_success() {
        let batch = batch(&[3]);
        let mut consumer = GenerationPopulation::default();
        assert!(consumer.admit(&batch));
        assert_eq!(consumer.process(accepted, |_| false).deferred, 1);
        assert!(batch.is_pending());
        assert_eq!(consumer.process(accepted, |_| true).spawned, 1);
        assert!(!batch.is_pending());
    }

    #[test]
    fn candidate_budget_rotates_deferred_batches_without_starvation() {
        let mut consumer = GenerationPopulation::default();
        let first = batch(&vec![3; CANDIDATE_BUDGET]);
        let second = batch(&[7]);
        assert!(consumer.admit(&first));
        assert!(consumer.admit(&second));
        let classify = |raw: &GenerationSpawn| {
            if raw.x == 3 { PlacementDecision::Deferred } else { accepted(raw) }
        };
        assert_eq!(consumer.process(classify, |_| true).deferred, CANDIDATE_BUDGET);
        let mut positions = Vec::new();
        consumer.process(classify, |spawn| { positions.push(spawn.pos.x); true });
        assert_eq!(positions, vec![7.5]);
        assert!(first.is_pending());
        assert!(!second.is_pending());
    }
}
