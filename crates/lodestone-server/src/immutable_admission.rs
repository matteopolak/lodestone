use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};
use lodestone_worldgen::structure::StructureStart;

use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

type Coordinate = (i32, i32);

pub(crate) struct AdmissionJob {
    generator: Arc<OverworldGenerator>,
    coordinates: Vec<Coordinate>,
    admitted: Vec<Coordinate>,
    prefix_targets: Vec<Coordinate>,
    prefix_radius: i32,
}

pub(crate) struct AdmissionProducts {
    pub(crate) columns: Vec<GeneratedColumn>,
    pub(crate) references: Vec<(Coordinate, BTreeMap<String, Vec<i64>>)>,
    pub(crate) starts: Vec<(Coordinate, Vec<Arc<StructureStart>>)>,
}

impl AdmissionJob {
    pub(crate) fn new(
        generator: Arc<OverworldGenerator>,
        coordinates: &[Coordinate],
        lease_coordinates: &[Coordinate],
        prefix_targets: &[Coordinate],
        prefix_radius: i32,
    ) -> Self {
        let admitted = lease_coordinates.iter().chain(prefix_targets).copied()
            .collect::<BTreeSet<_>>().into_iter().collect();
        Self {
            generator,
            coordinates: coordinates.to_vec(),
            admitted,
            prefix_targets: prefix_targets.to_vec(),
            prefix_radius,
        }
    }

    pub(crate) fn run(self) -> AdmissionProducts {
        let lease = {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::Lease, self.admitted.len() as u32);
            self.generator.lease_batch(&self.admitted)
        };
        {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::PreOre, self.prefix_targets.len() as u32,
            );
            #[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
            let tile_side = (crate::chunk::browser_worldgen_parallelism() > 1).then_some(4);
            #[cfg(not(all(target_arch = "wasm32", feature = "wasm-threads")))]
            let tile_side = None;
            let jobs = lease.pre_ore_region_work(&self.prefix_targets, self.prefix_radius, tile_side);
            let _prepared = crate::run_worldgen_jobs(jobs, |job| lease.prepare_pre_ore_region(job));
        }
        let mut references = Vec::with_capacity(self.prefix_targets.len());
        let mut starts = Vec::with_capacity(self.prefix_targets.len());
        {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::StructureContext, self.prefix_targets.len() as u32,
            );
            let mut origins = BTreeSet::new();
            for &(cx, cz) in &self.prefix_targets {
                let chunk_references = lease.structure_references(cx, cz);
                origins.extend(chunk_references.values().flatten().map(|packed| {
                    (*packed as u32 as i32, (*packed >> 32) as u32 as i32)
                }));
                references.push(((cx, cz), chunk_references));
                starts.push(((cx, cz), lease.structure_starts(cx, cz)));
            }
            for origin in origins {
                starts.push((origin, lease.structure_starts(origin.0, origin.1)));
            }
        }
        let columns = {
            let _timing = PhaseTimer::start(
                WorldgenTimingPhase::ShapedProducts, self.coordinates.len() as u32,
            );
            crate::run_worldgen_jobs(self.coordinates, |(cx, cz)| lease.column_shaped(cx, cz))
        };
        {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::Lease, self.admitted.len() as u32);
            drop(lease);
        }
        AdmissionProducts { columns, references, starts }
    }
}

#[cfg(any(all(target_arch = "wasm32", feature = "wasm-threads"), all(test, not(target_arch = "wasm32"))))]
mod threaded {
    use std::sync::{Arc, OnceLock};
    use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

    use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};
    use crate::worldgen_session::{RequestCancellation, SessionError};

    // One admission may fan out across the entire compute pool.
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

    pub(crate) struct Completed<T> {
        value: T,
        permit: OwnedSemaphorePermit,
        return_timer: Option<PhaseTimer>,
    }

    impl<T> Completed<T> {
        pub(crate) fn accept<R>(self, accept: impl FnOnce(T) -> R) -> R {
            let Self { value, permit, return_timer } = self;
            drop(return_timer);
            let result = accept(value);
            drop(permit);
            result
        }
    }

    pub(crate) async fn execute<F, T>(
        items: u32,
        cancellations: Vec<RequestCancellation>,
        work: F,
    ) -> Result<Completed<T>, SessionError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permits = Arc::clone(PERMITS.get_or_init(|| Arc::new(Semaphore::new(1))));
        execute_with(permits, items, cancellations, work, |job| rayon::spawn(job)).await
    }

    async fn execute_with<F, T>(
        permits: Arc<Semaphore>,
        items: u32,
        cancellations: Vec<RequestCancellation>,
        work: F,
        submit: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) -> Result<Completed<T>, SessionError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        super::check_cancellations(&cancellations)?;
        let queue_timer = PhaseTimer::start(WorldgenTimingPhase::ImmutableQueueWait, items);
        let permit = permits.acquire_owned().await.map_err(|_| {
            SessionError::InvalidCheckpointAt("immutable admission dispatcher closed")
        })?;
        let (sender, receiver) = oneshot::channel();
        submit(Box::new(move || {
            drop(queue_timer);
            if sender.is_closed() {
                return;
            }
            if let Err(error) = super::check_cancellations(&cancellations) {
                let _ = sender.send(Err(error));
                return;
            }
            let compute_timer = PhaseTimer::start(WorldgenTimingPhase::ImmutableCompute, items);
            let value = work();
            drop(compute_timer);
            let return_timer = PhaseTimer::start(WorldgenTimingPhase::ImmutableReturn, items);
            let _ = sender.send(Ok(Completed { value, permit, return_timer }));
        }));
        receiver.await.map_err(|_| {
            SessionError::InvalidCheckpointAt("immutable admission worker closed without a result")
        })?
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    mod tests {
        use super::*;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_services_timer_while_worker_is_gated() {
            let permits = Arc::new(Semaphore::new(1));
            let (entered, running) = oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let finished = Arc::new(AtomicBool::new(false));
            let worker_finished = Arc::clone(&finished);
            let task = tokio::spawn(execute_with(permits, 1, Vec::new(), move || {
                let _ = entered.send(());
                let _ = gate.recv();
                worker_finished.store(true, Ordering::Release);
                7
            }, |job| rayon::spawn(job)));
            tokio::time::timeout(Duration::from_secs(5), running).await.unwrap().unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(!finished.load(Ordering::Acquire));
            release.send(()).unwrap();
            assert_eq!(task.await.unwrap().unwrap().accept(|value| value), 7);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_cancelled_receiver_keeps_running_permit() {
            let permits = Arc::new(Semaphore::new(1));
            let (entered, running) = oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let task = tokio::spawn(execute_with(Arc::clone(&permits), 1, Vec::new(), move || {
                let _ = entered.send(());
                let _ = gate.recv();
            }, |job| rayon::spawn(job)));
            tokio::time::timeout(Duration::from_secs(5), running).await.unwrap().unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(permits.available_permits(), 0);
            release.send(()).unwrap();
            let permit = tokio::time::timeout(Duration::from_secs(5), permits.acquire())
                .await.unwrap().unwrap();
            drop(permit);
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_completed_result_retains_permit_and_order() {
            use rayon::prelude::*;
            let permits = Arc::new(Semaphore::new(1));
            let completed = execute_with(Arc::clone(&permits), 3, Vec::new(), || {
                vec![3, 1, 2].into_par_iter().map(|value| value * 10).collect::<Vec<_>>()
            }, |job| rayon::spawn(job)).await.unwrap();
            assert_eq!(permits.available_permits(), 0);
            completed.accept(|values| {
                assert_eq!(values, vec![30, 10, 20]);
                assert_eq!(permits.available_permits(), 0);
            });
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_cancellation_before_start_skips_work() {
            let permits = Arc::new(Semaphore::new(1));
            let ran = Arc::new(AtomicBool::new(false));
            let worker_ran = Arc::clone(&ran);
            let (submitted, queued) = oneshot::channel();
            let task = tokio::spawn(execute_with(Arc::clone(&permits), 1, Vec::new(), move || {
                worker_ran.store(true, Ordering::Release);
            }, move |job| { let _ = submitted.send(job); }));
            let job = queued.await.unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(permits.available_permits(), 0);
            job();
            assert!(!ran.load(Ordering::Acquire));
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_closed_worker_is_an_error() {
            let permits = Arc::new(Semaphore::new(1));
            let result = execute_with(Arc::clone(&permits), 1, Vec::new(), || 7, drop).await;
            assert!(matches!(result, Err(SessionError::InvalidCheckpointAt(
                "immutable admission worker closed without a result",
            ))));
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_cancelled_tokens_skip_queued_work() {
            let permits = Arc::new(Semaphore::new(1));
            let cancellation = RequestCancellation::new();
            let ran = Arc::new(AtomicBool::new(false));
            let worker_ran = Arc::clone(&ran);
            let (submitted, queued) = oneshot::channel();
            let task = tokio::spawn(execute_with(
                Arc::clone(&permits), 1, vec![cancellation.clone()],
                move || worker_ran.store(true, Ordering::Release),
                move |job| { let _ = submitted.send(job); },
            ));
            let job = queued.await.unwrap();
            cancellation.cancel();
            job();
            assert!(matches!(task.await.unwrap(), Err(SessionError::Cancelled)));
            assert!(!ran.load(Ordering::Acquire));
            assert_eq!(permits.available_permits(), 1);
        }

    }
}

#[cfg(any(all(target_arch = "wasm32", feature = "wasm-threads"), all(test, not(target_arch = "wasm32"))))]
pub(crate) use threaded::execute;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn check_cancellations(
    cancellations: &[crate::worldgen_session::RequestCancellation],
) -> Result<(), crate::worldgen_session::SessionError> {
    if !cancellations.is_empty() && cancellations.iter().all(|token| token.is_cancelled()) {
        Err(crate::worldgen_session::SessionError::Cancelled)
    } else {
        Ok(())
    }
}
