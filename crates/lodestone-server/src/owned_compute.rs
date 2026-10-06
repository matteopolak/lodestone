#[cfg(any(all(target_arch = "wasm32", feature = "wasm-threads"), all(test, not(target_arch = "wasm32"))))]
mod threaded {
    use std::sync::Arc;
    #[cfg(target_arch = "wasm32")]
    use std::sync::OnceLock;
    use tokio::sync::{oneshot, OwnedSemaphorePermit, Semaphore};

    use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};
    use crate::worldgen_session::{RequestCancellation, SessionError};

    #[cfg(target_arch = "wasm32")]
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

    #[derive(Clone, Copy)]
    pub(crate) enum OwnedJobRole {
        GenerationAdmission,
        PacketPreparation,
        TargetFeatures,
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

    impl OwnedJobRole {
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
                (Self::TargetFeatures, JobStage::PermitWait) => Phase::TargetFeaturesPermitWait,
                (Self::TargetFeatures, JobStage::PoolWait) => Phase::TargetFeaturesPoolWait,
                (Self::TargetFeatures, JobStage::Compute) => Phase::TargetFeaturesCompute,
                (Self::TargetFeatures, JobStage::ReturnWait) => Phase::TargetFeaturesReturnWait,
                (Self::TargetFeatures, JobStage::Acceptance) => Phase::TargetFeaturesAcceptance,
                (Self::TargetFeatures, JobStage::PermitHold) => Phase::TargetFeaturesPermitHold,
            }
        }
    }

    type TimerStart = fn(WorldgenTimingPhase, u32) -> Option<PhaseTimer>;

    fn check_cancellations(cancellations: &[RequestCancellation]) -> Result<(), SessionError> {
        if !cancellations.is_empty() && cancellations.iter().all(RequestCancellation::is_cancelled) {
            Err(SessionError::Cancelled)
        } else {
            Ok(())
        }
    }

    struct TimedPermit {
        _timing: Option<PhaseTimer>,
        _permit: OwnedSemaphorePermit,
    }

    pub(crate) struct Completed<T> {
        value: T,
        return_timers: (Option<PhaseTimer>, Option<PhaseTimer>),
        permit: TimedPermit,
        role: OwnedJobRole,
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

    #[cfg(target_arch = "wasm32")]
    pub(crate) async fn execute<F, T>(
        role: OwnedJobRole,
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
        role: OwnedJobRole,
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
        role: OwnedJobRole,
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
        check_cancellations(&cancellations)?;
        let immutable = !matches!(role, OwnedJobRole::TargetFeatures);
        let queue_timer = immutable.then(|| start_timer(WorldgenTimingPhase::ImmutableQueueWait, items)).flatten();
        let permit_wait_timer = start_timer(role.phase(JobStage::PermitWait), items);
        let permit = permits.acquire_owned().await.map_err(|_| {
            SessionError::InvalidCheckpointAt("owned compute dispatcher closed")
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
            if let Err(error) = check_cancellations(&cancellations) {
                let _ = sender.send(Err(error));
                return;
            }
            let compute_timers = (
                immutable.then(|| start_timer(WorldgenTimingPhase::ImmutableCompute, items)).flatten(),
                start_timer(role.phase(JobStage::Compute), items),
            );
            let value = work();
            drop(compute_timers);
            let return_timers = (
                immutable.then(|| start_timer(WorldgenTimingPhase::ImmutableReturn, items)).flatten(),
                start_timer(role.phase(JobStage::ReturnWait), items),
            );
            let _ = sender.send(Ok(Completed {
                value, return_timers, permit, role, items, start_timer,
            }));
        }));
        receiver.await.map_err(|_| {
            SessionError::InvalidCheckpointAt("owned compute worker closed without a result")
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
            let task = tokio::spawn(execute_with(permits, OwnedJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
            let task = tokio::spawn(execute_with(Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
            let completed = execute_with(Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 3, Vec::new(), || {
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
            let task = tokio::spawn(execute_with(Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 1, Vec::new(), move || {
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
                Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 1, Vec::new(), || 7, drop,
            ).await;
            assert!(matches!(result, Err(SessionError::InvalidCheckpointAt(
                "owned compute worker closed without a result",
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
                Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 1, vec![cancellation.clone()],
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
                Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 2,
                vec![cancelled.clone(), live], || 29,
                move |job| { let _ = submitted.send(job); },
            ));
            let job = queued.await.unwrap();
            cancelled.cancel();
            job();
            assert_eq!(task.await.unwrap().unwrap().accept(|value| value), 29);
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn target_feature_queued_cancel_and_unaccepted_drop_release_one_permit() {
            let permits = Arc::new(Semaphore::new(1));
            let cancellation = RequestCancellation::new();
            let ran = Arc::new(AtomicBool::new(false));
            let worker_ran = Arc::clone(&ran);
            let (submitted, queued) = oneshot::channel();
            let task = tokio::spawn(execute_with(
                Arc::clone(&permits), OwnedJobRole::TargetFeatures, 1, vec![cancellation.clone()],
                move || worker_ran.store(true, Ordering::Release),
                move |job| { let _ = submitted.send(job); },
            ));
            let job = queued.await.unwrap();
            cancellation.cancel();
            job();
            assert!(matches!(task.await.unwrap(), Err(SessionError::Cancelled)));
            assert!(!ran.load(Ordering::Acquire));
            assert_eq!(permits.available_permits(), 1);
            let completed = execute_with(
                Arc::clone(&permits), OwnedJobRole::TargetFeatures, 1, Vec::new(), || 43,
                |job| rayon::spawn(job),
            ).await.unwrap();
            assert_eq!(permits.available_permits(), 0);
            drop(completed);
            assert_eq!(permits.available_permits(), 1);
        }

        #[tokio::test(flavor = "current_thread")]
        async fn target_feature_dropped_running_receiver_releases_after_body() {
            let permits = Arc::new(Semaphore::new(1));
            let (entered, running) = oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let task = tokio::spawn(execute_with(
                Arc::clone(&permits), OwnedJobRole::TargetFeatures, 1, Vec::new(), move || {
                    let _ = entered.send(());
                    let _ = gate.recv_timeout(Duration::from_secs(5));
                }, |job| rayon::spawn(job),
            ));
            running.await.unwrap();
            task.abort();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            assert_eq!(permits.available_permits(), 0);
            release.send(()).unwrap();
            let permit = tokio::time::timeout(Duration::from_secs(5), permits.acquire()).await.unwrap().unwrap();
            drop(permit);
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

        #[tokio::test(flavor = "current_thread")]
        async fn target_feature_timing_keeps_owned_body_out_of_immutable_totals() {
            use WorldgenTimingPhase as Phase;
            let permits = Arc::new(Semaphore::new(1));
            let completed = execute_with_timers(
                Arc::clone(&permits), OwnedJobRole::TargetFeatures, 706, Vec::new(),
                || 47, |job| job(), observed_timer,
            ).await.unwrap();
            assert_eq!(completed.accept(|value| value), 47);
            assert_eq!(permits.available_permits(), 1);
            let events = timing_events(706);
            for phase in [
                Phase::TargetFeaturesPermitWait, Phase::TargetFeaturesPoolWait,
                Phase::TargetFeaturesCompute, Phase::TargetFeaturesReturnWait,
                Phase::TargetFeaturesAcceptance, Phase::TargetFeaturesPermitHold,
            ] {
                assert_eq!(events.iter().filter(|event| **event == (phase, true)).count(), 1);
                assert_eq!(events.iter().filter(|event| **event == (phase, false)).count(), 1);
            }
            assert!(!events.iter().any(|(phase, _)| matches!(
                phase, Phase::ImmutableQueueWait | Phase::ImmutableCompute | Phase::ImmutableReturn,
            )));
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
                Arc::clone(&permits), OwnedJobRole::GenerationAdmission, 701, Vec::new(),
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
                    Arc::clone(&permits), OwnedJobRole::PacketPreparation,
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
                Arc::clone(&permits), OwnedJobRole::PacketPreparation, 705, Vec::new(),
                || -> i32 { panic!("authored worker panic") },
                |job| {
                    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err());
                },
                observed_timer,
            ).await;
            assert!(matches!(result, Err(SessionError::InvalidCheckpointAt(
                "owned compute worker closed without a result",
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

#[cfg(all(target_arch = "wasm32", feature = "wasm-threads"))]
pub(crate) use threaded::{execute, OwnedJobRole};
