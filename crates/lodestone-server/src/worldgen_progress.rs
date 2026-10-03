use std::sync::{Mutex, OnceLock};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum WorldgenTimingPhase {
    Lease,
    PreOre,
    StructureContext,
    ShapedProducts,
    PrefixImport,
    MutableSettlement,
    SnapshotAssembly,
    PacketLighting,
    PacketEncoding,
    WireSend,
    BrowserYield,
    ImmutableQueueWait,
    ImmutableCompute,
    ImmutableReturn,
    GenerationPoll,
    EncodePoll,
    ConnectionPoll,
    ConnectionDispatch,
    ResidentLightSettlement,
    ResidentLightCompute,
    ResidentLightEncode,
    ConnectionRelight,
    GenerationAdmissionPermitWait,
    GenerationAdmissionPoolWait,
    GenerationAdmissionCompute,
    GenerationAdmissionReturnWait,
    GenerationAdmissionAcceptance,
    GenerationAdmissionPermitHold,
    PacketPreparationPermitWait,
    PacketPreparationPoolWait,
    PacketPreparationCompute,
    PacketPreparationReturnWait,
    PacketPreparationAcceptance,
    PacketPreparationPermitHold,
    PreOreSlots,
    PreOreInitializations,
    PreOreReuses,
    PreOreBatches,
    PreOreEvaluatedPrefixes,
    ReplayPreparation,
    ReplayEpochSetup,
    TargetFeaturesPermitWait,
    TargetFeaturesPoolWait,
    TargetFeaturesCompute,
    TargetFeaturesReturnWait,
    TargetFeaturesAcceptance,
    TargetFeaturesPermitHold,
}

impl WorldgenTimingPhase {
    pub const ALL: [Self; 47] = [
        Self::Lease,
        Self::PreOre,
        Self::StructureContext,
        Self::ShapedProducts,
        Self::PrefixImport,
        Self::MutableSettlement,
        Self::SnapshotAssembly,
        Self::PacketLighting,
        Self::PacketEncoding,
        Self::WireSend,
        Self::BrowserYield,
        Self::ImmutableQueueWait,
        Self::ImmutableCompute,
        Self::ImmutableReturn,
        Self::GenerationPoll,
        Self::EncodePoll,
        Self::ConnectionPoll,
        Self::ConnectionDispatch,
        Self::ResidentLightSettlement,
        Self::ResidentLightCompute,
        Self::ResidentLightEncode,
        Self::ConnectionRelight,
        Self::GenerationAdmissionPermitWait,
        Self::GenerationAdmissionPoolWait,
        Self::GenerationAdmissionCompute,
        Self::GenerationAdmissionReturnWait,
        Self::GenerationAdmissionAcceptance,
        Self::GenerationAdmissionPermitHold,
        Self::PacketPreparationPermitWait,
        Self::PacketPreparationPoolWait,
        Self::PacketPreparationCompute,
        Self::PacketPreparationReturnWait,
        Self::PacketPreparationAcceptance,
        Self::PacketPreparationPermitHold,
        Self::PreOreSlots,
        Self::PreOreInitializations,
        Self::PreOreReuses,
        Self::PreOreBatches,
        Self::PreOreEvaluatedPrefixes,
        Self::ReplayPreparation,
        Self::ReplayEpochSetup,
        Self::TargetFeaturesPermitWait,
        Self::TargetFeaturesPoolWait,
        Self::TargetFeaturesCompute,
        Self::TargetFeaturesReturnWait,
        Self::TargetFeaturesAcceptance,
        Self::TargetFeaturesPermitHold,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Lease => "lease",
            Self::PreOre => "pre-ore",
            Self::StructureContext => "structure-context",
            Self::ShapedProducts => "shaped-products",
            Self::PrefixImport => "prefix-import",
            Self::MutableSettlement => "mutable-settlement",
            Self::SnapshotAssembly => "snapshot-assembly",
            Self::PacketLighting => "packet-lighting",
            Self::PacketEncoding => "packet-encoding",
            Self::WireSend => "wire-send",
            Self::BrowserYield => "browser-yield",
            Self::ImmutableQueueWait => "immutable-queue-wait",
            Self::ImmutableCompute => "immutable-compute",
            Self::ImmutableReturn => "immutable-return",
            Self::GenerationPoll => "generation-poll",
            Self::EncodePoll => "encode-poll",
            Self::ConnectionPoll => "connection-poll",
            Self::ConnectionDispatch => "connection-dispatch",
            Self::ResidentLightSettlement => "resident-light-settlement",
            Self::ResidentLightCompute => "resident-light-compute",
            Self::ResidentLightEncode => "resident-light-encode",
            Self::ConnectionRelight => "connection-relight",
            Self::GenerationAdmissionPermitWait => "generation-admission-permit-wait",
            Self::GenerationAdmissionPoolWait => "generation-admission-pool-wait",
            Self::GenerationAdmissionCompute => "generation-admission-compute",
            Self::GenerationAdmissionReturnWait => "generation-admission-return-wait",
            Self::GenerationAdmissionAcceptance => "generation-admission-acceptance",
            Self::GenerationAdmissionPermitHold => "generation-admission-permit-hold",
            Self::PacketPreparationPermitWait => "packet-preparation-permit-wait",
            Self::PacketPreparationPoolWait => "packet-preparation-pool-wait",
            Self::PacketPreparationCompute => "packet-preparation-compute",
            Self::PacketPreparationReturnWait => "packet-preparation-return-wait",
            Self::PacketPreparationAcceptance => "packet-preparation-acceptance",
            Self::PacketPreparationPermitHold => "packet-preparation-permit-hold",
            Self::PreOreSlots => "pre-ore-slots",
            Self::PreOreInitializations => "pre-ore-initializations",
            Self::PreOreReuses => "pre-ore-reuses",
            Self::PreOreBatches => "pre-ore-batches",
            Self::PreOreEvaluatedPrefixes => "pre-ore-evaluated-prefixes",
            Self::ReplayPreparation => "replay-preparation",
            Self::ReplayEpochSetup => "replay-epoch-setup",
            Self::TargetFeaturesPermitWait => "target-features-permit-wait",
            Self::TargetFeaturesPoolWait => "target-features-pool-wait",
            Self::TargetFeaturesCompute => "target-features-compute",
            Self::TargetFeaturesReturnWait => "target-features-return-wait",
            Self::TargetFeaturesAcceptance => "target-features-acceptance",
            Self::TargetFeaturesPermitHold => "target-features-permit-hold",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct WorldgenTimingSample {
    pub phase: WorldgenTimingPhase,
    pub elapsed: Duration,
    pub items: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct WorldgenTimingTotals {
    pub calls: u64,
    pub items: u64,
    pub elapsed: Duration,
    pub maximum: Duration,
}

impl WorldgenTimingTotals {
    const EMPTY: Self = Self {
        calls: 0,
        items: 0,
        elapsed: Duration::ZERO,
        maximum: Duration::ZERO,
    };

    pub fn record(&mut self, sample: WorldgenTimingSample) {
        self.calls = self.calls.saturating_add(1);
        self.items = self.items.saturating_add(u64::from(sample.items));
        self.elapsed = self.elapsed.saturating_add(sample.elapsed);
        self.maximum = self.maximum.max(sample.elapsed);
    }
}

#[derive(Debug)]
pub struct WorldgenTimingBuffer {
    totals: Mutex<[WorldgenTimingTotals; WorldgenTimingPhase::ALL.len()]>,
}

impl Default for WorldgenTimingBuffer {
    fn default() -> Self { Self::new() }
}

impl WorldgenTimingBuffer {
    pub const fn new() -> Self {
        Self {
            totals: Mutex::new([WorldgenTimingTotals::EMPTY; WorldgenTimingPhase::ALL.len()]),
        }
    }

    pub fn record(&self, sample: WorldgenTimingSample) {
        self.totals.lock().expect("worldgen timing buffer poisoned")
            [sample.phase.index()].record(sample);
    }

    pub fn drain(&self) -> [WorldgenTimingTotals; WorldgenTimingPhase::ALL.len()] {
        std::mem::replace(
            &mut *self.totals.lock().expect("worldgen timing buffer poisoned"),
            [WorldgenTimingTotals::EMPTY; WorldgenTimingPhase::ALL.len()],
        )
    }

    pub fn restore(&self, previous: [WorldgenTimingTotals; WorldgenTimingPhase::ALL.len()]) {
        let mut totals = self.totals.lock().expect("worldgen timing buffer poisoned");
        for (current, previous) in totals.iter_mut().zip(previous) {
            current.calls = current.calls.saturating_add(previous.calls);
            current.items = current.items.saturating_add(previous.items);
            current.elapsed = current.elapsed.saturating_add(previous.elapsed);
            current.maximum = current.maximum.max(previous.maximum);
        }
    }
}

pub type WorldgenTimingSink = fn(WorldgenTimingSample);

static TIMING_SINK: OnceLock<WorldgenTimingSink> = OnceLock::new();

pub fn install_timing_sink(sink: WorldgenTimingSink) -> Result<(), WorldgenTimingSink> {
    TIMING_SINK.set(sink)
}

pub(crate) fn record_work(phase: WorldgenTimingPhase, items: usize) {
    if let Some(sink) = TIMING_SINK.get() {
        sink(WorldgenTimingSample {
            phase,
            elapsed: Duration::ZERO,
            items: items.min(u32::MAX as usize) as u32,
        });
    }
}

pub(crate) struct PhaseTimer {
    sink: WorldgenTimingSink,
    phase: WorldgenTimingPhase,
    started: lodestone_time::Instant,
    items: u32,
}

impl PhaseTimer {
    pub(crate) fn start(phase: WorldgenTimingPhase, items: u32) -> Option<Self> {
        let sink = *TIMING_SINK.get()?;
        Some(Self {
            sink,
            phase,
            started: lodestone_time::Instant::now(),
            items,
        })
    }

    #[cfg(all(test, not(target_arch = "wasm32")))]
    pub(crate) fn with_sink(
        phase: WorldgenTimingPhase,
        items: u32,
        sink: WorldgenTimingSink,
    ) -> Self {
        Self { sink, phase, started: lodestone_time::Instant::now(), items }
    }
}

impl Drop for PhaseTimer {
    fn drop(&mut self) {
        (self.sink)(WorldgenTimingSample {
            phase: self.phase,
            elapsed: self.started.elapsed(),
            items: self.items,
        });
    }
}

/// Measures occupied lighting work without including cooperative host waits.
pub fn time_resident_light_step<T>(items: u32, work: impl FnOnce() -> T) -> T {
    let _timing = PhaseTimer::start(WorldgenTimingPhase::ResidentLightCompute, items);
    work()
}

pub(crate) fn measure_polls<F: std::future::Future>(
    phase: WorldgenTimingPhase,
    mut future: std::pin::Pin<&mut F>,
) -> impl std::future::Future<Output = F::Output> {
    std::future::poll_fn(move |cx| {
        let _timing = PhaseTimer::start(phase, 1);
        future.as_mut().poll(cx)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorldgenProgress {
    pub session: u64,
    pub target: (i32, i32),
    pub admitted: u32,
    pub completed: u32,
    pub committed: u32,
    pub queued: u32,
    pub retained_bytes: usize,
    pub stage: &'static str,
}

impl WorldgenProgress {
    #[cfg(any(target_arch = "wasm32", test))]
    pub(crate) fn wire_delivered(target: (i32, i32), delivered: usize, remaining: usize) -> Self {
        let count = |value: usize| value.min(u32::MAX as usize) as u32;
        Self {
            session: 0,
            target,
            admitted: count(delivered.saturating_add(remaining)),
            completed: count(delivered),
            committed: 0,
            queued: count(remaining),
            retained_bytes: 0,
            stage: "wire-delivered",
        }
    }
}

pub type WorldgenProgressSink = fn(WorldgenProgress);

static SINK: OnceLock<WorldgenProgressSink> = OnceLock::new();

pub fn install_sink(sink: WorldgenProgressSink) -> Result<(), WorldgenProgressSink> {
    SINK.set(sink)
}

pub(crate) fn emit(progress: WorldgenProgress) {
    if let Some(sink) = SINK.get() {
        sink(progress);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorldgenProgress, WorldgenTimingBuffer, WorldgenTimingPhase, WorldgenTimingSample,
        WorldgenTimingTotals,
    };
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_phase_buffer_keeps_worker_samples_and_failed_delivery() {
        let buffer = std::sync::Arc::new(WorldgenTimingBuffer::new());
        let mut jobs = Vec::new();
        for micros in [17, 29] {
            let buffer = std::sync::Arc::clone(&buffer);
            jobs.push(tokio::spawn(async move {
                for _ in 0..127 {
                    buffer.record(WorldgenTimingSample {
                        phase: WorldgenTimingPhase::ImmutableCompute,
                        elapsed: Duration::from_micros(micros),
                        items: 3,
                    });
                    tokio::task::yield_now().await;
                }
            }));
        }
        for job in jobs { job.await.unwrap(); }
        let drained = buffer.drain();
        assert_eq!(drained[WorldgenTimingPhase::ImmutableCompute.index()], WorldgenTimingTotals {
            calls: 254,
            items: 762,
            elapsed: Duration::from_micros(5_842),
            maximum: Duration::from_micros(29),
        });
        buffer.record(WorldgenTimingSample {
            phase: WorldgenTimingPhase::ImmutableCompute,
            elapsed: Duration::from_micros(31),
            items: 5,
        });
        buffer.restore(drained);
        assert_eq!(buffer.drain()[WorldgenTimingPhase::ImmutableCompute.index()], WorldgenTimingTotals {
            calls: 255,
            items: 767,
            elapsed: Duration::from_micros(5_873),
            maximum: Duration::from_micros(31),
        });
        assert!(buffer.drain().iter().all(|totals| totals.calls == 0));
    }

    #[test]
    fn phase_samples_keep_operation_counts_separate_from_elapsed_totals() {
        let mut totals = [WorldgenTimingTotals::default(); WorldgenTimingPhase::ALL.len()];
        for (phase, items, micros) in [
            (WorldgenTimingPhase::PreOre, 9, 2_300),
            (WorldgenTimingPhase::PreOre, 1, 700),
            (WorldgenTimingPhase::PacketLighting, 1, 1_200),
        ] {
            totals[phase.index()].record(WorldgenTimingSample {
                phase,
                items,
                elapsed: Duration::from_micros(micros),
            });
        }
        assert_eq!(
            totals[WorldgenTimingPhase::PreOre.index()],
            WorldgenTimingTotals {
                calls: 2,
                items: 10,
                elapsed: Duration::from_micros(3_000),
                maximum: Duration::from_micros(2_300),
            },
        );
        assert_eq!(totals[WorldgenTimingPhase::PacketLighting.index()].calls, 1);
        assert_eq!(
            totals[WorldgenTimingPhase::PacketEncoding.index()],
            WorldgenTimingTotals::default(),
        );
        let drained = std::mem::replace(
            &mut totals,
            [WorldgenTimingTotals::default(); WorldgenTimingPhase::ALL.len()],
        );
        assert_eq!(drained[WorldgenTimingPhase::PreOre.index()].calls, 2);
        assert_eq!(
            totals,
            [WorldgenTimingTotals::default(); WorldgenTimingPhase::ALL.len()],
        );
        for (index, phase) in WorldgenTimingPhase::ALL.into_iter().enumerate() {
            assert_eq!(phase.index(), index);
        }
    }

    #[test]
    fn wire_progress_counts_delivered_and_outstanding_targets_separately() {
        let progress = WorldgenProgress::wire_delivered((-3, 5), 17, 24);
        assert_eq!(progress.target, (-3, 5));
        assert_eq!((progress.admitted, progress.completed, progress.queued), (41, 17, 24));
        let finished = WorldgenProgress::wire_delivered((-3, 5), 41, 0);
        assert_eq!((finished.admitted, finished.completed, finished.queued), (41, 41, 0));
    }
}
