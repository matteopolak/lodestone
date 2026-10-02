use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use lodestone_worldgen::end::EndGenerationIdentity;
use lodestone_worldgen::nether::NetherGenerationIdentity;
use lodestone_worldgen::overworld::{GeneratedColumn, OverworldGenerator};
use lodestone_worldgen::stage_schedule::{
    GeneratedProducerIdentity, GeneratedStageIdentity, GenerationTarget, PipelineOptions,
    StageIdentity, END_PIPELINE, NETHER_PIPELINE,
};
use lodestone_worldgen::structure::StructureStart;

use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

type Coordinate = (i32, i32);

pub struct OwnedAdmissionWork(Box<dyn FnOnce() -> OwnedAdmissionProducts + Send>);

impl OwnedAdmissionWork {
    pub(crate) fn new(work: impl FnOnce() -> OwnedAdmissionProducts + Send + 'static) -> Self {
        Self(Box::new(work))
    }

    pub(crate) fn run(self) -> OwnedAdmissionProducts { (self.0)() }
}

pub enum AdmissionColumn {
    Generated(GeneratedColumn),
    Materialized(crate::chunk::ChunkColumn),
}

pub(crate) struct AdmissionMetadata {
    pub(crate) boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    pub(crate) content: Option<AdmissionContentMetadata>,
    pub(crate) client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    pub(crate) references: Option<BTreeMap<String, Vec<i64>>>,
}

pub(crate) struct AdmissionContentMetadata {
    pub(crate) fingerprint: StageIdentity,
    pub(crate) retained_bytes: usize,
}

pub struct OwnedAdmissionProducts {
    pub(crate) columns: Vec<(Coordinate, AdmissionColumn, AdmissionMetadata)>,
    pub(crate) context: Option<AdmissionProducts>,
}

pub(crate) fn materialized_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    materialized_product_with_fingerprint(coordinate, boundary, column, client_heightmaps, references, None)
}

pub(crate) fn pristine_end_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    identity: &EndGenerationIdentity,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let fingerprint = pristine_end_identity(
        identity, coordinate, boundary, &column,
        EndGenerationIdentity::SHAPED_VERSION,
        crate::production_worldgen_session::EXECUTOR_VERSION,
    );
    materialized_product_with_fingerprint(
        coordinate, boundary, column, client_heightmaps, None, Some(fingerprint),
    )
}

fn pristine_end_identity(
    identity: &EndGenerationIdentity,
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: &crate::chunk::ChunkColumn,
    shaped_version: u32,
    executor_version: u32,
) -> StageIdentity {
    pristine_product_identity(
        GeneratedProducerIdentity::End(identity.clone()), coordinate, boundary, column,
        shaped_version, executor_version,
    )
}

pub(crate) fn pristine_nether_product(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
    identity: &NetherGenerationIdentity,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let fingerprint = pristine_product_identity(
        GeneratedProducerIdentity::Nether(identity.clone()), coordinate, boundary, &column,
        NetherGenerationIdentity::SHAPED_VERSION,
        crate::production_worldgen_session::EXECUTOR_VERSION,
    );
    materialized_product_with_fingerprint(
        coordinate, boundary, column, client_heightmaps, references, Some(fingerprint),
    )
}

fn pristine_product_identity(
    producer: GeneratedProducerIdentity,
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: &crate::chunk::ChunkColumn,
    product_version: u32,
    executor_version: u32,
) -> StageIdentity {
    let pipeline = match &producer {
        GeneratedProducerIdentity::End(_) => END_PIPELINE,
        GeneratedProducerIdentity::Nether(_) => NETHER_PIPELINE,
    };
    StageIdentity::Generated(Arc::new(GeneratedStageIdentity {
        producer,
        coordinate,
        pipeline: pipeline.identity(PipelineOptions::ALL),
        boundary,
        carrier_target: match column.generation_stage() {
            crate::chunk::ChunkGenerationStage::Shaped => GenerationTarget::Shaped,
            crate::chunk::ChunkGenerationStage::Full => GenerationTarget::Full,
        },
        min_y: column.min_y,
        height: column.height,
        executor_version,
        product_version,
    }))
}

fn materialized_product_with_fingerprint(
    coordinate: Coordinate,
    boundary: lodestone_worldgen::stage_schedule::ColumnStage,
    column: crate::chunk::ChunkColumn,
    client_heightmaps: Option<crate::worldgen_lifecycle::LifecycleClientHeightmaps>,
    references: Option<BTreeMap<String, Vec<i64>>>,
    fingerprint: Option<StageIdentity>,
) -> (Coordinate, AdmissionColumn, AdmissionMetadata) {
    let metadata = AdmissionMetadata {
        boundary,
        content: Some(AdmissionContentMetadata {
            fingerprint: fingerprint.unwrap_or_else(|| {
                crate::production_worldgen_session::column_fingerprint(coordinate, boundary, &column).into()
            }),
            retained_bytes: column.memory_census().logical_total(),
        }),
        client_heightmaps,
        references,
    };
    (coordinate, AdmissionColumn::Materialized(column), metadata)
}

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

    #[derive(Clone, Copy)]
    pub(crate) enum ImmutableJobRole {
        GenerationAdmission,
        PacketPreparation,
    }

    #[derive(Clone, Copy)]
    enum JobStage {
        PermitWait,
        PoolWait,
        Compute,
        ReturnWait,
        Acceptance,
        PermitHold,
    }

    impl ImmutableJobRole {
        fn phase(self, stage: JobStage) -> WorldgenTimingPhase {
            use WorldgenTimingPhase as Phase;
            match (self, stage) {
                (Self::GenerationAdmission, JobStage::PermitWait) => Phase::GenerationAdmissionPermitWait,
                (Self::GenerationAdmission, JobStage::PoolWait) => Phase::GenerationAdmissionPoolWait,
                (Self::GenerationAdmission, JobStage::Compute) => Phase::GenerationAdmissionCompute,
                (Self::GenerationAdmission, JobStage::ReturnWait) => Phase::GenerationAdmissionReturnWait,
                (Self::GenerationAdmission, JobStage::Acceptance) => Phase::GenerationAdmissionAcceptance,
                (Self::GenerationAdmission, JobStage::PermitHold) => Phase::GenerationAdmissionPermitHold,
                (Self::PacketPreparation, JobStage::PermitWait) => Phase::PacketPreparationPermitWait,
                (Self::PacketPreparation, JobStage::PoolWait) => Phase::PacketPreparationPoolWait,
                (Self::PacketPreparation, JobStage::Compute) => Phase::PacketPreparationCompute,
                (Self::PacketPreparation, JobStage::ReturnWait) => Phase::PacketPreparationReturnWait,
                (Self::PacketPreparation, JobStage::Acceptance) => Phase::PacketPreparationAcceptance,
                (Self::PacketPreparation, JobStage::PermitHold) => Phase::PacketPreparationPermitHold,
            }
        }
    }

    type TimerStart = fn(WorldgenTimingPhase, u32) -> Option<PhaseTimer>;

    struct TimedPermit {
        _timing: Option<PhaseTimer>,
        _permit: OwnedSemaphorePermit,
    }

    pub(crate) struct Completed<T> {
        value: T,
        return_timers: (Option<PhaseTimer>, Option<PhaseTimer>),
        permit: TimedPermit,
        role: ImmutableJobRole,
        items: u32,
        start_timer: TimerStart,
    }

    impl<T> Completed<T> {
        pub(crate) fn accept<R>(self, accept: impl FnOnce(T) -> R) -> R {
            let Self { value, return_timers, permit, role, items, start_timer } = self;
            drop(return_timers);
            let acceptance_timer = start_timer(role.phase(JobStage::Acceptance), items);
            let result = accept(value);
            drop(acceptance_timer);
            drop(permit);
            result
        }
    }

    pub(crate) async fn execute<F, T>(
        role: ImmutableJobRole,
        items: u32,
        cancellations: Vec<RequestCancellation>,
        work: F,
    ) -> Result<Completed<T>, SessionError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permits = Arc::clone(PERMITS.get_or_init(|| Arc::new(Semaphore::new(1))));
        execute_with(permits, role, items, cancellations, work, |job| rayon::spawn(job)).await
    }

    async fn execute_with<F, T>(
        permits: Arc<Semaphore>,
        role: ImmutableJobRole,
        items: u32,
        cancellations: Vec<RequestCancellation>,
        work: F,
        submit: impl FnOnce(Box<dyn FnOnce() + Send>),
    ) -> Result<Completed<T>, SessionError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        execute_with_timers(
            permits, role, items, cancellations, work, submit, PhaseTimer::start,
        ).await
    }

    async fn execute_with_timers<F, T>(
        permits: Arc<Semaphore>,
        role: ImmutableJobRole,
        items: u32,
        cancellations: Vec<RequestCancellation>,
        work: F,
        submit: impl FnOnce(Box<dyn FnOnce() + Send>),
        start_timer: TimerStart,
    ) -> Result<Completed<T>, SessionError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        super::check_cancellations(&cancellations)?;
        let queue_timer = start_timer(WorldgenTimingPhase::ImmutableQueueWait, items);
        let permit_wait_timer = start_timer(role.phase(JobStage::PermitWait), items);
        let permit = permits.acquire_owned().await.map_err(|_| {
            SessionError::InvalidCheckpointAt("immutable admission dispatcher closed")
        })?;
        drop(permit_wait_timer);
        let permit = TimedPermit {
            _timing: start_timer(role.phase(JobStage::PermitHold), items),
            _permit: permit,
        };
        let (sender, receiver) = oneshot::channel();
        let pool_wait_timer = start_timer(role.phase(JobStage::PoolWait), items);
        submit(Box::new(move || {
            drop(pool_wait_timer);
            drop(queue_timer);
            if sender.is_closed() {
                return;
            }
            if let Err(error) = super::check_cancellations(&cancellations) {
                let _ = sender.send(Err(error));
                return;
            }
            let compute_timers = (
                start_timer(WorldgenTimingPhase::ImmutableCompute, items),
                start_timer(role.phase(JobStage::Compute), items),
            );
            let value = work();
            drop(compute_timers);
            let return_timers = (
                start_timer(WorldgenTimingPhase::ImmutableReturn, items),
                start_timer(role.phase(JobStage::ReturnWait), items),
            );
            let _ = sender.send(Ok(Completed {
                value, return_timers, permit, role, items, start_timer,
            }));
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
            let task = tokio::spawn(execute_with(permits, ImmutableJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
            let task = tokio::spawn(execute_with(Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
            let completed = execute_with(Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 3, Vec::new(), || {
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
            let task = tokio::spawn(execute_with(Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
            let result = execute_with(
                Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 1, Vec::new(), || 7, drop,
            ).await;
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
                Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 1, vec![cancellation.clone()],
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

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_admission_keeps_live_member_of_queued_batch() {
            let permits = Arc::new(Semaphore::new(1));
            let cancelled = RequestCancellation::new();
            let live = RequestCancellation::new();
            let (submitted, queued) = oneshot::channel();
            let task = tokio::spawn(execute_with(
                Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 2,
                vec![cancelled.clone(), live], || 29,
                move |job| { let _ = submitted.send(job); },
            ));
            let job = queued.await.unwrap();
            cancelled.cancel();
            job();
            assert_eq!(task.await.unwrap().unwrap().accept(|value| value), 29);
            assert_eq!(permits.available_permits(), 1);
        }

        static TIMING_EVENTS: std::sync::Mutex<Vec<(u32, WorldgenTimingPhase, bool)>> =
            std::sync::Mutex::new(Vec::new());

        fn observed_timer(phase: WorldgenTimingPhase, items: u32) -> Option<PhaseTimer> {
            TIMING_EVENTS.lock().unwrap().push((items, phase, true));
            Some(PhaseTimer::with_sink(phase, items, |sample| {
                TIMING_EVENTS.lock().unwrap().push((sample.items, sample.phase, false));
            }))
        }

        fn timing_events(items: u32) -> Vec<(WorldgenTimingPhase, bool)> {
            TIMING_EVENTS.lock().unwrap().iter()
                .filter(|event| event.0 == items).map(|event| (event.1, event.2)).collect()
        }

        #[test]
        fn immutable_admission_timing_separates_permit_and_pool_wait() {
            use std::future::Future;
            use std::task::{Context, Poll, Waker};
            use WorldgenTimingPhase as Phase;

            let permits = Arc::new(Semaphore::new(1));
            let held = Arc::clone(&permits).try_acquire_owned().unwrap();
            let (submitted, queued) = std::sync::mpsc::channel();
            let mut future = std::pin::pin!(execute_with_timers(
                Arc::clone(&permits), ImmutableJobRole::GenerationAdmission, 701, Vec::new(),
                || 7, move |job| submitted.send(job).unwrap(), observed_timer,
            ));
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::GenerationAdmissionPermitWait, true),
            ]);
            assert!(queued.try_recv().is_err());
            drop(held);
            assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::GenerationAdmissionPermitWait, true),
                (Phase::GenerationAdmissionPermitWait, false),
                (Phase::GenerationAdmissionPermitHold, true),
                (Phase::GenerationAdmissionPoolWait, true),
            ]);
            queued.try_recv().unwrap()();
            let Poll::Ready(Ok(completed)) = future.as_mut().poll(&mut context) else {
                panic!("authored worker did not return its result");
            };
            assert_eq!(permits.available_permits(), 0);
            assert_eq!(completed.accept(|value| value), 7);
            assert_eq!(permits.available_permits(), 1);
            assert_eq!(timing_events(701), vec![
                (Phase::ImmutableQueueWait, true),
                (Phase::GenerationAdmissionPermitWait, true),
                (Phase::GenerationAdmissionPermitWait, false),
                (Phase::GenerationAdmissionPermitHold, true),
                (Phase::GenerationAdmissionPoolWait, true),
                (Phase::GenerationAdmissionPoolWait, false),
                (Phase::ImmutableQueueWait, false),
                (Phase::ImmutableCompute, true),
                (Phase::GenerationAdmissionCompute, true),
                (Phase::ImmutableCompute, false),
                (Phase::GenerationAdmissionCompute, false),
                (Phase::ImmutableReturn, true),
                (Phase::GenerationAdmissionReturnWait, true),
                (Phase::ImmutableReturn, false),
                (Phase::GenerationAdmissionReturnWait, false),
                (Phase::GenerationAdmissionAcceptance, true),
                (Phase::GenerationAdmissionAcceptance, false),
                (Phase::GenerationAdmissionPermitHold, false),
            ]);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn immutable_packet_timing_closes_on_acceptance_discard_and_panic() {
            use WorldgenTimingPhase as Phase;

            for (items, discard, panic_in_accept) in [
                (702, false, false), (703, true, false), (704, false, true),
            ] {
                let permits = Arc::new(Semaphore::new(1));
                let completed = execute_with_timers(
                    Arc::clone(&permits), ImmutableJobRole::PacketPreparation,
                    items, Vec::new(), || 11, |job| job(), observed_timer,
                ).await.unwrap();
                assert_eq!(permits.available_permits(), 0);
                assert_eq!(timing_events(items).last(), Some(&(Phase::PacketPreparationReturnWait, true)));
                if discard {
                    drop(completed);
                } else {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        completed.accept(|value| {
                            assert_eq!(value, 11);
                            assert_eq!(permits.available_permits(), 0);
                            let events = timing_events(items);
                            assert!(events.contains(&(Phase::PacketPreparationReturnWait, false)));
                            assert_eq!(events.last(), Some(&(Phase::PacketPreparationAcceptance, true)));
                            assert!(!events.contains(&(Phase::PacketPreparationPermitHold, false)));
                            assert!(!panic_in_accept, "authored acceptance panic");
                        });
                    }));
                    assert_eq!(result.is_err(), panic_in_accept);
                }
                assert_eq!(permits.available_permits(), 1);
                let events = timing_events(items);
                assert_eq!(events.last(), Some(&(Phase::PacketPreparationPermitHold, false)));
                assert_eq!(events.contains(&(Phase::PacketPreparationAcceptance, true)), !discard);
                for phase in [
                    Phase::ImmutableQueueWait, Phase::ImmutableCompute, Phase::ImmutableReturn,
                    Phase::PacketPreparationPermitWait, Phase::PacketPreparationPoolWait,
                    Phase::PacketPreparationCompute, Phase::PacketPreparationReturnWait,
                    Phase::PacketPreparationPermitHold,
                ] {
                    assert_eq!(events.iter().filter(|event| **event == (phase, true)).count(), 1);
                    assert_eq!(events.iter().filter(|event| **event == (phase, false)).count(), 1);
                }
                assert_eq!(events.contains(&(Phase::PacketPreparationAcceptance, false)), !discard);
            }

            let permits = Arc::new(Semaphore::new(1));
            let result = execute_with_timers(
                Arc::clone(&permits), ImmutableJobRole::PacketPreparation, 705, Vec::new(),
                || -> i32 { panic!("authored worker panic") },
                |job| {
                    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err());
                },
                observed_timer,
            ).await;
            assert!(matches!(result, Err(SessionError::InvalidCheckpointAt(
                "immutable admission worker closed without a result",
            ))));
            assert_eq!(permits.available_permits(), 1);
            let events = timing_events(705);
            for phase in [
                Phase::ImmutableQueueWait, Phase::ImmutableCompute,
                Phase::PacketPreparationPermitWait, Phase::PacketPreparationPoolWait,
                Phase::PacketPreparationCompute, Phase::PacketPreparationPermitHold,
            ] {
                assert_eq!(events.iter().filter(|event| **event == (phase, true)).count(), 1);
                assert_eq!(events.iter().filter(|event| **event == (phase, false)).count(), 1);
            }
            assert!(!events.iter().any(|(phase, _)| matches!(
                phase, Phase::ImmutableReturn | Phase::PacketPreparationReturnWait |
                Phase::PacketPreparationAcceptance,
            )));
        }

    }
}

#[cfg(any(all(target_arch = "wasm32", feature = "wasm-threads"), all(test, not(target_arch = "wasm32"))))]
pub(crate) use threaded::{execute, ImmutableJobRole};

pub(crate) fn check_cancellations(
    cancellations: &[crate::worldgen_session::RequestCancellation],
) -> Result<(), crate::worldgen_session::SessionError> {
    if !cancellations.is_empty() && cancellations.iter().all(|token| token.is_cancelled()) {
        Err(crate::worldgen_session::SessionError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use crate::chunk::{ChunkColumn, ChunkGenerationStage, ChunkSource, EndChunkSource};
    use crate::worldgen_lifecycle::{LifecycleCompletion, LifecycleMaterializer, LifecycleWorldgenSource};
    use lodestone_data::block::Block;
    use lodestone_worldgen::stage_schedule::{ColumnStage, Dimension, GenerationTarget, StageKey, END};

    fn fingerprint(metadata: &AdmissionMetadata) -> StageIdentity {
        metadata.content.as_ref().expect("unchanged admission content").fingerprint.clone()
    }

    fn content_identity(coordinate: Coordinate, boundary: ColumnStage, column: &ChunkColumn) -> StageIdentity {
        crate::production_worldgen_session::column_fingerprint(coordinate, boundary, column).into()
    }

    #[test]
    fn pristine_end_product_identity_is_independent_and_domain_complete() {
        let first = crate::worldgen_data::end_chunk_source(42);
        let independent = crate::worldgen_data::end_chunk_source(42);
        let coordinates = [(0, 0), (1, -2)];
        let first = first.owned_admission_work(&coordinates).run();
        let independent = independent.owned_admission_work(&coordinates).run();
        let mut ledger = crate::chunk_store::GenerationLedger::new();
        ledger.admit(END_PIPELINE, &coordinates).unwrap();
        for ((coordinate, column, metadata), (_, other, other_metadata)) in first.columns.iter().zip(&independent.columns) {
            let AdmissionColumn::Materialized(column) = column else { panic!("End carrier") };
            let AdmissionColumn::Materialized(other) = other else { panic!("End carrier") };
            assert_eq!(fingerprint(metadata), fingerprint(other_metadata));
            assert_eq!(metadata.client_heightmaps, other_metadata.client_heightmaps);
            assert_eq!(metadata.content.as_ref().unwrap().retained_bytes, column.memory_census().logical_total());
            assert_eq!(
                crate::production_worldgen_session::column_fingerprint(*coordinate, metadata.boundary, column),
                crate::production_worldgen_session::column_fingerprint(*coordinate, metadata.boundary, other),
            );
            assert!(matches!(fingerprint(metadata), StageIdentity::Generated(_)));
            assert_ne!(fingerprint(metadata), content_identity(
                *coordinate, metadata.boundary, column,
            ));
            let make_session = |column: &ChunkColumn, identity: StageIdentity| {
                let mut session = crate::worldgen_session::GenerationSession::new(
                    crate::worldgen_session::GenerationRequest::new(
                        Dimension::End, *coordinate, GenerationTarget::Full, 0,
                    ),
                );
                session.import_aggregate_prefix(
                    *coordinate, metadata.boundary,
                    crate::worldgen_session::ImmutableProduct::new(
                        lodestone_worldgen::stage_schedule::ResourceKey::MaterializedWorld,
                        column.clone(),
                    ),
                    [], &identity, &identity,
                    crate::production_worldgen_session::EXECUTOR_VERSION,
                ).unwrap();
                session
            };
            let session = make_session(column, fingerprint(metadata));
            ledger.publish_session(END_PIPELINE, &session).unwrap();
            let same = make_session(other, fingerprint(other_metadata));
            ledger.publish_session(END_PIPELINE, &same).unwrap();
            let checkpoint = ledger.checkpoint(END_PIPELINE, session.request()).unwrap();
            let restored = crate::worldgen_session::GenerationSession::from_checkpoint(checkpoint).unwrap();
            ledger.publish_session(END_PIPELINE, &restored).unwrap();
            let StageIdentity::Generated(mut changed) = fingerprint(metadata) else { unreachable!() };
            Arc::make_mut(&mut changed).product_version += 1;
            let divergent = make_session(column, StageIdentity::Generated(changed));
            assert_eq!(ledger.publish_session(END_PIPELINE, &divergent),
                Err(crate::chunk_store::GenerationLedgerError::CheckpointMismatch));
        }
        assert_ne!(fingerprint(&first.columns[0].2), fingerprint(&first.columns[1].2));

        let generator = crate::worldgen_data::end_generator(42);
        let boundary = END.target_stage(GenerationTarget::Shaped);
        let column = EndChunkSource::new(generator).shaped_column(0, 0)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        let generator = crate::worldgen_data::end_generator(42);
        let identity = generator.generation_identity().unwrap();
        let shaped_version = EndGenerationIdentity::SHAPED_VERSION;
        let executor_version = crate::production_worldgen_session::EXECUTOR_VERSION;
        let base = pristine_end_product((0, 0), boundary, column.clone(), None, identity);
        for (shaped, executor) in [
            (shaped_version + 1, executor_version),
            (shaped_version, executor_version + 1),
        ] {
            assert_ne!(fingerprint(&base.2), pristine_end_identity(
                identity, (0, 0), boundary, &column, shaped, executor,
            ));
        }
        let shifted = ChunkColumn::new(-16, 256)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        let shorter = ChunkColumn::new(0, 128)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        for (coordinate, boundary, column) in [
            ((-1, 0), boundary, column.clone()),
            ((0, 0), ColumnStage::Output, column.clone()),
            ((0, 0), boundary, shifted),
            ((0, 0), boundary, shorter),
            ((0, 0), boundary, column.test_with_generation_stage(ChunkGenerationStage::Full)),
        ] {
            assert_ne!(fingerprint(&base.2), pristine_end_identity(
                identity, coordinate, boundary, &column, shaped_version, executor_version,
            ));
        }
    }

    #[test]
    fn dynamic_end_admission_hashes_its_actual_content() {
        use lodestone_worldgen::density::{NoiseParams, Resolver};

        struct DynamicResolver;
        impl Resolver for DynamicResolver {
            fn density_function(&self, _id: &str) -> serde_json::Value { unreachable!("constant density") }
            fn noise(&self, _id: &str) -> NoiseParams {
                NoiseParams { first_octave: -6, amplitudes: vec![1.0] }
            }
        }
        let settings = serde_json::json!({
            "legacy_random_source": true,
            "noise": { "min_y": 0, "height": 128, "size_horizontal": 2, "size_vertical": 1 },
            "sea_level": 0,
            "default_block": { "Name": "minecraft:end_stone" },
            "default_fluid": { "Name": "minecraft:air" },
            "noise_router": { "final_density": 0.5, "preliminary_surface_level": 0.0 },
            "surface_rule": { "type": "minecraft:sequence", "sequence": [] }
        });
        let generator = lodestone_worldgen::end::EndGenerator::new(42, &settings, &DynamicResolver);
        assert!(generator.generation_identity().is_none());
        let products = EndChunkSource::new(generator).owned_admission_work(&[(0, 0)]).run();
        let (_, column, metadata) = &products.columns[0];
        let AdmissionColumn::Materialized(column) = column else { panic!("End carrier") };
        assert_eq!(fingerprint(metadata), content_identity(
            (0, 0), metadata.boundary, column,
        ));
        let mut changed = column.clone();
        changed.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
        assert_ne!(fingerprint(metadata), content_identity(
            (0, 0), metadata.boundary, &changed,
        ));
    }

    #[test]
    fn authoritative_admission_keeps_exact_content_identity() {
        let source = crate::worldgen_data::end_chunk_source(42);
        let coordinate = (100, -100);
        let mut input = ChunkColumn::new(0, EndChunkSource::WINDOW_HEIGHT)
            .test_with_generation_stage(ChunkGenerationStage::Shaped);
        input.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
        source.retain_generation_input(coordinate.0, coordinate.1, &input);
        let retained = source.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), content_identity(
            coordinate, retained.columns[0].2.boundary, &input,
        ));
        let previous = fingerprint(&retained.columns[0].2);
        source.set_block(coordinate.0 * 16 + 3, 201, coordinate.1 * 16 + 5, Block::DiamondBlock.default_state());
        let edited = source.owned_admission_work(&[coordinate]).run();
        let AdmissionColumn::Materialized(column) = &edited.columns[0].1 else { panic!("End carrier") };
        assert_eq!(fingerprint(&edited.columns[0].2), content_identity(
            coordinate, edited.columns[0].2.boundary, column,
        ));
        assert_ne!(previous, fingerprint(&edited.columns[0].2));

        let nether = crate::worldgen_data::nether_chunk_source(42);
        let pristine = nether.owned_admission_work(&[coordinate]).run();
        assert!(matches!(fingerprint(&pristine.columns[0].2), StageIdentity::Generated(_)));
        nether.retain_generation_input(coordinate.0, coordinate.1, &input);
        let retained = nether.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), content_identity(
            coordinate, retained.columns[0].2.boundary, &input,
        ));
        let independent = crate::worldgen_data::nether_chunk_source(42);
        independent.retain_generation_input(coordinate.0, coordinate.1, &input);
        let independent = independent.owned_admission_work(&[coordinate]).run();
        assert_eq!(fingerprint(&retained.columns[0].2), fingerprint(&independent.columns[0].2));
    }

    #[test]
    fn pristine_end_identity_revokes_for_foreign_and_restored_writes() {
        use crate::worldgen_session::{BlockCoordinate, ProvenanceMutation};
        use std::cell::Cell;

        let source = crate::worldgen_data::end_chunk_source(42);
        let source_chunk = (6, 0);
        let destination = (6, -1);
        let boundary = END.target_stage(GenerationTarget::Shaped);
        for restored in [false, true] {
            let mut materializer = LifecycleMaterializer::new(&source);
            materializer.admit_many_parallel(&[source_chunk, destination]);
            let original = materializer.shared_resident_prefix(destination, boundary, |_| {
                panic!("pristine End admission must carry its producer identity")
            }).unwrap();
            if restored {
                let mutation = ProvenanceMutation::test_block_state(
                    source_chunk, source_chunk, StageKey::new(Dimension::End, ColumnStage::Features),
                    0, BlockCoordinate::new(100, 201, -1), 1, Block::GoldBlock.default_state(),
                );
                materializer.restore_committed_mutations([&mutation]);
            } else {
                materializer.complete(source_chunk, LifecycleCompletion::Features, 0);
            }
            let scanned = Cell::new(false);
            let changed = materializer.shared_resident_prefix(destination, boundary, |column| {
                scanned.set(true);
                crate::production_worldgen_session::column_fingerprint(destination, boundary, column)
            }).unwrap();
            assert!(scanned.get(), "foreign/restored writes must revoke pristine content identity");
            assert_ne!(original.1, changed.1);
            assert_eq!(changed.0.block_state_id(4, if restored { 201 } else { 48 }, 15),
                if restored { Block::GoldBlock.default_state() } else { Block::Obsidian.default_state() });
            assert_eq!(changed.1, content_identity(destination, boundary, &changed.0));
        }
    }

    #[test]
    fn end_pristine_admission_retries_after_input_replacement() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ReplacingSource {
            source: Arc<EndChunkSource>,
            attempts: Arc<AtomicUsize>,
        }

        impl LifecycleWorldgenSource for ReplacingSource {
            type ReplayContext = ();
            fn lifecycle_replay_context(&self, _target: Coordinate) -> Arc<()> { Arc::new(()) }
            fn shaped_column(&self, cx: i32, cz: i32) -> ChunkColumn { self.source.shaped_column(cx, cz) }
            fn immutable_admission_versions(&self, chunks: &[Coordinate]) -> Vec<Option<u64>> {
                self.source.immutable_admission_versions(chunks)
            }
            fn owned_admission_work(
                &self, chunks: &[Coordinate], _lease: &[Coordinate], _targets: &[Coordinate], _radius: i32,
            ) -> Option<OwnedAdmissionWork> {
                let work = self.source.owned_admission_work(chunks);
                let source = Arc::clone(&self.source);
                let attempts = Arc::clone(&self.attempts);
                let coordinate = chunks[0];
                Some(OwnedAdmissionWork::new(move || {
                    let products = work.run();
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        let mut replacement = ChunkColumn::new(0, EndChunkSource::WINDOW_HEIGHT)
                            .test_with_generation_stage(ChunkGenerationStage::Shaped);
                        replacement.set_block_id(3, 201, 5, Block::GoldBlock.default_state());
                        source.retain_generation_input(coordinate.0, coordinate.1, &replacement);
                    }
                    products
                }))
            }
            fn feature_result(
                &self, _source: Coordinate,
                _overrides: &BTreeMap<(i32, i32, i32), lodestone_data::block_states::StateId>,
                _resident: &BTreeMap<Coordinate, ChunkColumn>,
            ) -> crate::worldgen_lifecycle::LifecycleFeatureResult { unreachable!("admission-only control") }
        }

        let source = ReplacingSource {
            source: Arc::new(crate::worldgen_data::end_chunk_source(42)),
            attempts: Arc::new(AtomicUsize::new(0)),
        };
        let mut materializer = LifecycleMaterializer::new(&source);
        materializer.admit_many_parallel(&[(0, 0)]);
        assert_eq!(source.attempts.load(Ordering::SeqCst), 2);
        let column = materializer.resident_column((0, 0)).unwrap();
        assert_eq!(column.block_state_id(3, 201, 5), Block::GoldBlock.default_state());
        let boundary = END.target_stage(GenerationTarget::Shaped);
        let prefix = materializer.shared_resident_prefix((0, 0), boundary, |_| {
            panic!("replacement metadata must be accepted after retry")
        }).unwrap();
        assert_eq!(prefix.1, content_identity((0, 0), boundary, &prefix.0));
    }
}
