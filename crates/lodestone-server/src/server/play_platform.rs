//! The seams where a native and a browser connection differ, so `serve_play` is one loop: its
//! timers, its time-bounded stream wait, off-loop relighting, join batching, the stall watch and
//! the browser progress probe.

use super::*;

/// Converts the loop's elapsed time into a tick count at the normal 20 TPS, for the dig clock and
/// the time-of-day broadcast.
pub(super) fn ticks_since(start: crate::tick::PlayTimerInstant) -> i64 {
    (start.elapsed().as_millis() / MILLIS_PER_TICK) as i64
}

/// Columns a join-stream chunk batch holds before it is closed. Native batches them so the client's
/// rate estimate has something to measure; a browser presents each column as it arrives.
pub(super) const JOIN_BATCH_COLUMNS: usize = if cfg!(target_arch = "wasm32") {
    1
} else {
    JOIN_STREAM_BATCH_COLUMNS
};

/// A periodic timer for the play loop: tokio's interval on native, the host timer in a browser,
/// where tokio's never fires. The first tick is one period out, so no timer fires in the instant
/// the join sequence finished.
pub(super) struct PlayInterval {
    #[cfg(not(target_arch = "wasm32"))]
    inner: tokio::time::Interval,
    #[cfg(target_arch = "wasm32")]
    inner: Option<crate::browser_timer::BrowserInterval>,
}

impl PlayInterval {
    /// Missed periods collapse into one tick, so an overrun yields one pass over unchanged state.
    pub(super) fn delayed(period: Duration) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut inner = tokio::time::interval_at(crate::tick::PlayTimerInstant::now() + period, period);
            inner.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            Self { inner }
        }
        #[cfg(target_arch = "wasm32")]
        Self { inner: Some(crate::browser_timer::BrowserInterval::new(period)) }
    }

    /// Missed periods fire back to back on native, so a stalled vitals pass catches up; a browser
    /// timer always collapses them.
    pub(super) fn catching_up(period: Duration) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self { inner: tokio::time::interval_at(crate::tick::PlayTimerInstant::now() + period, period) }
        }
        #[cfg(target_arch = "wasm32")]
        Self::delayed(period)
    }

    /// The keep-alive challenge timer. A browser connection is an in-process stream whose peer
    /// cannot go quiet independently, so there it never fires.
    pub(super) fn keep_alive(period: Duration) -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::delayed(period)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = period;
            Self { inner: None }
        }
    }

    /// Cancel-safe: a dropped wait leaves the deadline untouched.
    pub(super) async fn tick(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.inner.tick().await;
        }
        #[cfg(target_arch = "wasm32")]
        match self.inner.as_mut() {
            Some(interval) => interval.tick().await,
            None => std::future::pending().await,
        }
    }
}

/// Awaits `future` for at most `budget`, so a slow column cannot keep the socket-read arm from
/// being polled. `None` means the budget ran out; the dropped future must be safe to poll again.
pub(super) async fn within_budget<F: std::future::Future>(budget: Duration, future: F) -> Option<F::Output> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        tokio::time::timeout(budget, future).await.ok()
    }
    #[cfg(target_arch = "wasm32")]
    {
        tokio::select! {
            output = future => Some(output),
            () = lodestone_time::browser_sleep(budget) => None,
        }
    }
}

/// A pass through the loop's `select!` shorter than this is not a stall: one server tick. Passes at
/// or above it are summed into [`LoopStallWatch::unserviced`] and the worst one is remembered.
pub(super) const STALL_FLOOR: Duration = Duration::from_millis(MILLIS_PER_TICK as u64);

pub(super) const STALL_REPORT: Duration = Duration::from_millis(200);

/// How long one pass through the loop's `select!` took, and which arm took it.
///
/// `select!` services one arm per pass, so while an arm runs this connection reads and writes
/// nothing even though the task is alive. The maximum needs to be named with its site, and the
/// keep-alive timeout must not count time the client could not have been heard in:
/// [`unserviced`](Self::unserviced) is what makes the deadline mean what a tick-driven server's
/// does, where reads never block on world generation. Denominating it in wall clock would kick a
/// healthy client for a reply the loop never gave itself the chance to read, and the kick reads on
/// the client as its own fault.
pub(super) struct LoopStallWatch {
    /// When the arm body currently running started; `None` between passes. Set at the top of the
    /// arm body, not at the bottom of the previous one: the interval between two passes is time
    /// parked in `select!`, where the loop is idle and the socket is being serviced. Timing
    /// pass-to-pass reports the whole keep-alive interval as a stall under a paused-clock runtime.
    pub(super) arm_start: Option<crate::tick::PlayTimerInstant>,
    /// The longest arm body observed, and its arm; `""` until one exceeds [`STALL_FLOOR`].
    pub(super) worst: Duration,
    pub(super) worst_arm: &'static str,
    /// Time spent unable to service the socket, summed over every arm body past [`STALL_FLOOR`],
    /// reset by [`clear_unserviced`](Self::clear_unserviced) when a keep-alive challenge is
    /// written so it answers "how much of this challenge's window did we eat".
    pub(super) unserviced: Duration,
}

impl LoopStallWatch {
    pub(super) fn new() -> Self {
        Self {
            arm_start: None,
            worst: Duration::ZERO,
            worst_arm: "",
            unserviced: Duration::ZERO,
        }
    }

    /// Opens a pass. Called as the first statement of every `select!` arm body.
    pub(super) fn enter(&mut self) {
        self.arm_start = Some(crate::tick::PlayTimerInstant::now());
    }

    /// Closes the pass that `arm` serviced. A no-op without a matching [`enter`](Self::enter), so
    /// an arm that returns early is not measured rather than charged someone else's time.
    pub(super) fn pass(&mut self, arm: &'static str) {
        let Some(start) = self.arm_start.take() else {
            return;
        };
        let took = start.elapsed();
        if took < STALL_FLOOR {
            return;
        }
        self.unserviced += took;
        if took > self.worst {
            self.worst = took;
            self.worst_arm = arm;
        }
        if took >= STALL_REPORT {
            tracing::warn!(
                target: "lodestone_server::stall",
                arm,
                millis = took.as_millis() as u64,
                "connection loop serviced nothing for one pass",
            );
        }
    }

    pub(super) fn clear_unserviced(&mut self) {
        self.unserviced = Duration::ZERO;
    }

    /// The worst pass, for a log line on the way out.
    pub(super) fn worst(&self) -> Option<(&'static str, Duration)> {
        (!self.worst_arm.is_empty()).then_some((self.worst_arm, self.worst))
    }
}

/// The browser's connection progress probe; nothing on native, where [`LoopStallWatch`] reports.
pub(super) struct PlayProbe {
    #[cfg(target_arch = "wasm32")]
    probe: Option<crate::connection_progress::ConnectionProbe>,
}

impl PlayProbe {
    pub(super) fn start() -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            probe: crate::connection_progress::ConnectionProbe::start(),
        }
    }

    /// Reports one pass; `progress` is only built when a probe is listening.
    pub(super) fn observe(&mut self, progress: impl FnOnce() -> crate::connection_progress::ConnectionProgress) {
        #[cfg(target_arch = "wasm32")]
        if let Some(probe) = self.probe.as_mut() {
            probe.observe(progress());
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = progress;
    }

    /// Reports a column on the wire to the browser's loading screen.
    pub(super) fn wire_delivered(&self, target: (i32, i32), delivered: usize, remaining: usize, drained: bool) {
        #[cfg(target_arch = "wasm32")]
        if delivered == 1 || delivered.is_multiple_of(16) || drained {
            crate::worldgen_progress::emit(crate::worldgen_progress::WorldgenProgress::wire_delivered(
                target, delivered, remaining,
            ));
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (target, delivered, remaining, drained);
    }

    pub(super) fn activity(
        &self,
        activity: crate::connection_progress::ConnectionActivity,
        target: Option<(i32, i32)>,
        packet_id: Option<i32>,
    ) {
        #[cfg(target_arch = "wasm32")]
        if let Some(probe) = self.probe.as_ref() {
            probe.activity(activity, target, packet_id);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = (activity, target, packet_id);
    }
}

#[cfg(not(target_arch = "wasm32"))]
type RelightFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = RelightBatchOutcome> + Send + 'a>>;
#[cfg(target_arch = "wasm32")]
type RelightFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = RelightBatchOutcome> + 'a>>;

struct RelightJob<'a> {
    batch: RelightBatch,
    future: RelightFuture<'a>,
}

/// Settles resident column light off the loop: on a blocking worker on native, as a cooperative
/// future the loop polls in a browser, which has no threads. Both feed the same completion path.
pub(super) struct RelightDriver<'a> {
    job: Option<RelightJob<'a>>,
    /// Whether the protocol can solve a batch cooperatively; cleared when the source declines.
    #[cfg(target_arch = "wasm32")]
    cooperative: bool,
}

impl<'a> RelightDriver<'a> {
    pub(super) fn new<P: ServerProtocol>(proto: &P) -> Self {
        #[cfg(target_arch = "wasm32")]
        let cooperative = proto.detached_resident_light_compute().is_some();
        #[cfg(not(target_arch = "wasm32"))]
        let _ = proto;
        Self {
            job: None,
            #[cfg(target_arch = "wasm32")]
            cooperative,
        }
    }

    pub(super) fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Abandons the job in flight, whose columns belong to a dimension being left.
    pub(super) fn clear(&mut self) {
        self.job = None;
    }

    /// Whether a packet dispatch hands its relight queue to this driver rather than settling light
    /// inline.
    pub(super) fn takes_queue<P: ServerProtocol, S: ChunkSource + 'static>(
        proto: &P,
        source: SourceRef<'_, S>,
    ) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            !matches!(source, SourceRef::Borrowed(_))
                && proto.retains_initial_column_light()
                && proto.detached_light_compute().is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = source;
            proto.retains_initial_column_light()
        }
    }

    /// Starts a job when the queue is due and none is running. Returns whether the queue may also
    /// be settled synchronously on this pass, which is only when the driver is not responsible.
    pub(super) fn pump<P: ServerProtocol, S: ChunkSource + 'static>(
        &mut self,
        proto: &'a P,
        source: SourceRef<'_, S>,
        home: SourceRef<'a, S>,
        pending: &mut PendingRelights,
        delivered: &HashSet<(i32, i32)>,
    ) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = home;
            let mut synchronous = true;
            let Some(coordinate) = pending.front().filter(|_| pending.ready()) else {
                return synchronous;
            };
            let (Some(shared), Some(compute)) = (
                source.shared_arc(),
                proto.detached_light_compute().filter(|_| proto.retains_initial_column_light()),
            ) else {
                return synchronous;
            };
            synchronous = false;
            if self.job.is_some() {
                return synchronous;
            }
            if !delivered.contains(&coordinate) {
                pending.pop_front();
                return synchronous;
            }
            let dimension = source.dimension();
            let cross_column = proto.uses_cross_column_light();
            let batch_compute = proto.detached_resident_light_compute();
            let batch = pending.batch(delivered, batch_compute.is_some());
            let coordinates = batch.coordinates.clone();
            let resident_only = !pending.requires_generation(coordinate);
            let work = move || {
                if coordinates.len() > 1
                    && let Some(batch_compute) = batch_compute
                {
                    let result =
                        compute_detached_relight_batch(shared.as_ref(), &coordinates, dimension, batch_compute);
                    if !matches!(result, RelightBatchOutcome::Unsupported | RelightBatchOutcome::Deferred) {
                        return result;
                    }
                }
                match compute_detached_relight(
                    shared.as_ref(),
                    coordinate,
                    dimension,
                    cross_column,
                    resident_only,
                    compute,
                ) {
                    Some(light) => RelightBatchOutcome::Committed(vec![(coordinate, light)]),
                    None => RelightBatchOutcome::Deferred,
                }
            };
            if let Ok(handle) = crate::worldgen_dispatch::try_spawn(work) {
                pending.admit(&batch);
                self.job = Some(RelightJob {
                    batch,
                    future: Box::pin(async move { handle.await.unwrap_or(RelightBatchOutcome::Deferred) }),
                });
            }
            synchronous
        }
        #[cfg(target_arch = "wasm32")]
        {
            if self.cooperative && self.job.is_none() && pending.ready() {
                if pending.front().is_some_and(|coordinate| !delivered.contains(&coordinate)) {
                    pending.pop_front();
                } else {
                    let batch = pending.batch(delivered, true);
                    let coordinates = batch.coordinates.clone();
                    pending.admit(&batch);
                    let owned_source = source.shared_arc();
                    let borrowed_home = home.get();
                    self.job = Some(RelightJob {
                        batch,
                        future: Box::pin(async move {
                            let source = owned_source.as_deref().unwrap_or(borrowed_home);
                            crate::worldgen_progress::measure_polls(
                                WorldgenTimingPhase::ConnectionRelight,
                                std::pin::pin!(compute_cooperative_relight(proto, source, coordinates)),
                            )
                            .await
                        }),
                    });
                }
            }
            !self.cooperative
        }
    }

    /// Resolves with the running job's outcome; pends forever when there is none, so it is only
    /// polled behind an [`is_running`](Self::is_running) precondition.
    pub(super) async fn finished(&mut self) -> RelightBatchOutcome {
        std::future::poll_fn(|cx| match self.job.as_mut() {
            Some(job) => job.future.as_mut().poll(cx),
            None => std::task::Poll::Pending,
        })
        .await
    }

    /// Takes the completed job's batch and writes what it committed.
    pub(super) async fn complete<T: Transport, P: ServerProtocol, S: ChunkSource + ?Sized>(
        &mut self,
        outcome: RelightBatchOutcome,
        conn: &mut Connection<T>,
        proto: &P,
        source: &S,
        state: &mut State,
        delivered: &HashSet<(i32, i32)>,
        pending: &mut PendingRelights,
    ) -> Result<(), ServerError> {
        let batch = self.job.take().expect("the completed relight owns its batch").batch;
        match outcome {
            RelightBatchOutcome::Committed(lights) => {
                let completed = lights.iter().map(|(coordinate, _)| *coordinate).collect::<Vec<_>>();
                let remainder = RelightBatch {
                    coordinates: batch
                        .coordinates
                        .into_iter()
                        .filter(|coordinate| !completed.contains(coordinate))
                        .collect(),
                    generation_required: batch.generation_required,
                };
                if !remainder.coordinates.is_empty() {
                    pending.defer(&remainder);
                }
                send_committed_relights(conn, proto, source, state, delivered, pending, lights).await?;
            }
            RelightBatchOutcome::Failed(error) => return Err(error.into()),
            RelightBatchOutcome::Unsupported => {
                #[cfg(target_arch = "wasm32")]
                {
                    self.cooperative = false;
                }
                pending.defer(&batch);
            }
            RelightBatchOutcome::Deferred => pending.defer(&batch),
        }
        Ok(())
    }
}
