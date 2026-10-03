use std::time::Duration;

use super::measurement::{MeshHandoffOutcome, MeshPhaseMeasurement};
use super::priority::MeshPriority;
use crate::platform::Instant;

/// Session totals; completed work contributes only when the scheduler settles it.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeMeshLaneTiming {
    pub submit_to_receive: MeshPhaseMeasurement,
    pub mesh_compute: MeshPhaseMeasurement,
    pub completed_to_upload: MeshPhaseMeasurement,
    pub upload_cpu: MeshPhaseMeasurement,
    pub built: u64,
    pub skipped: u64,
    pub stale: u64,
    pub applied: u64,
    pub unchanged: u64,
    pub failed: u64,
    pub invalid_timestamps: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NativeMeshTimingSnapshot {
    /// Background and edit lanes, in scheduler priority order.
    pub by_priority: [NativeMeshLaneTiming; 2],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct NativeMeshResultTiming {
    submit_to_receive: Option<Duration>,
    mesh_compute: Option<Duration>,
    completed_at: Instant,
    invalid_timestamps: u64,
}

impl NativeMeshResultTiming {
    pub(super) fn new(
        submitted_at: Instant,
        received_at: Instant,
        mesh_started_at: Option<Instant>,
        completed_at: Instant,
    ) -> Self {
        let submit_to_receive = received_at.checked_duration_since(submitted_at);
        let mesh_compute = mesh_started_at
            .and_then(|started| completed_at.checked_duration_since(started));
        let invalid_timestamps = u64::from(submit_to_receive.is_none())
            + u64::from(mesh_started_at.is_some() && mesh_compute.is_none());
        Self { submit_to_receive, mesh_compute, completed_at, invalid_timestamps }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum NativeMeshDisposition {
    Built,
    Skipped,
    Stale,
}

/// Fixed storage; per-job timing travels with the existing job and result payloads.
#[derive(Debug, Default)]
pub(super) struct NativeMeshTiming {
    enabled: bool,
    totals: NativeMeshTimingSnapshot,
}

impl NativeMeshTiming {
    pub(super) fn new(enabled: bool) -> Self {
        Self { enabled, totals: NativeMeshTimingSnapshot::default() }
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn snapshot(&self) -> NativeMeshTimingSnapshot {
        self.totals
    }

    pub(super) fn settled(
        &mut self,
        priority: MeshPriority,
        timing: Option<NativeMeshResultTiming>,
        disposition: NativeMeshDisposition,
    ) {
        if !self.enabled {
            return;
        }
        let Some(timing) = timing else { return };
        let totals = &mut self.totals.by_priority[priority.index()];
        if let Some(elapsed) = timing.submit_to_receive {
            totals.submit_to_receive.record(elapsed);
        }
        if let Some(elapsed) = timing.mesh_compute {
            totals.mesh_compute.record(elapsed);
        }
        totals.invalid_timestamps = totals.invalid_timestamps
            .saturating_add(timing.invalid_timestamps);
        match disposition {
            NativeMeshDisposition::Built => {
                totals.built = totals.built.saturating_add(1);
            }
            NativeMeshDisposition::Skipped => {
                totals.skipped = totals.skipped.saturating_add(1);
            }
            NativeMeshDisposition::Stale => {
                totals.built = totals.built.saturating_add(1);
                totals.stale = totals.stale.saturating_add(1);
            }
        }
    }

    pub(super) fn uploaded(
        &mut self,
        priority: MeshPriority,
        timing: Option<NativeMeshResultTiming>,
        upload_started_at: Instant,
        upload_finished_at: Instant,
        outcome: MeshHandoffOutcome,
    ) {
        if !self.enabled {
            return;
        }
        let Some(timing) = timing else { return };
        let totals = &mut self.totals.by_priority[priority.index()];
        match upload_started_at.checked_duration_since(timing.completed_at) {
            Some(elapsed) => totals.completed_to_upload.record(elapsed),
            None => totals.invalid_timestamps = totals.invalid_timestamps.saturating_add(1),
        }
        match upload_finished_at.checked_duration_since(upload_started_at) {
            Some(elapsed) => totals.upload_cpu.record(elapsed),
            None => totals.invalid_timestamps = totals.invalid_timestamps.saturating_add(1),
        }
        let count = match outcome {
            MeshHandoffOutcome::Applied => &mut totals.applied,
            MeshHandoffOutcome::Unchanged => &mut totals.unchanged,
            MeshHandoffOutcome::Failed => &mut totals.failed,
        };
        *count = count.saturating_add(1);
    }
}

impl std::fmt::Display for NativeMeshTimingSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mesh native:")?;
        for (index, name) in ["background", "edit"].into_iter().enumerate() {
            write!(f, " {name}=[{}]", self.by_priority[index])?;
        }
        Ok(())
    }
}

impl std::fmt::Display for NativeMeshLaneTiming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f,
            "built={} skipped={} stale={} applied={} unchanged={} failed={} invalid_times={}",
            self.built, self.skipped, self.stale, self.applied, self.unchanged,
            self.failed, self.invalid_timestamps,
        )?;
        for (name, phase) in [
            ("queue", self.submit_to_receive),
            ("compute", self.mesh_compute),
            ("ready", self.completed_to_upload),
            ("upload_cpu", self.upload_cpu),
        ] {
            write!(f, " {name}_calls={} {name}_total_ms={:.3} {name}_max_ms={:.3}",
                phase.calls, phase.total_ns as f64 / 1e6, phase.max_ns as f64 / 1e6,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(origin: Instant, millis: u64) -> Instant {
        origin + Duration::from_millis(millis)
    }

    fn single(millis: u64) -> MeshPhaseMeasurement {
        MeshPhaseMeasurement { calls: 1, total_ns: millis * 1_000_000, max_ns: millis * 1_000_000 }
    }

    #[test]
    fn separates_queue_compute_ready_residence_and_actual_upload() {
        let origin = Instant::now();
        let mut trace = NativeMeshTiming::new(true);
        let timing = Some(NativeMeshResultTiming::new(
            at(origin, 13), at(origin, 37), Some(at(origin, 37)), at(origin, 56),
        ));
        trace.settled(MeshPriority::Edit, timing, NativeMeshDisposition::Built);
        assert_eq!(trace.snapshot().by_priority[1].completed_to_upload.calls, 0);
        trace.uploaded(MeshPriority::Edit, timing, at(origin, 87), at(origin, 92), MeshHandoffOutcome::Unchanged);
        let measured = trace.snapshot().by_priority[1];
        assert_eq!(measured.submit_to_receive, single(24));
        assert_eq!(measured.mesh_compute, single(19));
        assert_eq!(measured.completed_to_upload, single(31));
        assert_eq!(measured.upload_cpu, single(5));
        assert_eq!((measured.built, measured.unchanged, measured.applied, measured.failed), (1, 1, 0, 0));
        assert_eq!(trace.snapshot().by_priority[0], NativeMeshLaneTiming::default());
    }

    #[test]
    fn skipped_and_stale_work_never_fabricate_upload_timings() {
        let origin = Instant::now();
        let mut trace = NativeMeshTiming::new(true);
        trace.settled(MeshPriority::Background, Some(NativeMeshResultTiming::new(
            at(origin, 13), at(origin, 37), None, at(origin, 56),
        )), NativeMeshDisposition::Skipped);
        assert_eq!(trace.snapshot().by_priority[0].mesh_compute.calls, 0);
        trace.settled(MeshPriority::Background, Some(NativeMeshResultTiming::new(
            at(origin, 19), at(origin, 32), Some(at(origin, 38)), at(origin, 49),
        )), NativeMeshDisposition::Stale);
        let measured = trace.snapshot().by_priority[0];
        assert_eq!(measured.submit_to_receive,
            MeshPhaseMeasurement { calls: 2, total_ns: 37_000_000, max_ns: 24_000_000 });
        assert_eq!(measured.mesh_compute, single(11));
        assert_eq!((measured.built, measured.skipped, measured.stale), (1, 1, 1));
        assert_eq!(measured.completed_to_upload, MeshPhaseMeasurement::default());
        assert_eq!(measured.upload_cpu, MeshPhaseMeasurement::default());
        assert_eq!(trace.snapshot().by_priority[1], NativeMeshLaneTiming::default());
        assert!(std::mem::size_of::<NativeMeshTimingSnapshot>() <= 320);
    }

    #[test]
    fn disabled_and_missing_samples_leave_totals_empty() {
        let origin = Instant::now();
        let timing = Some(NativeMeshResultTiming::new(origin, origin, Some(origin), origin));
        let mut disabled = NativeMeshTiming::new(false);
        assert!(!disabled.enabled());
        disabled.settled(MeshPriority::Background, timing, NativeMeshDisposition::Built);
        disabled.uploaded(MeshPriority::Background, timing, origin, origin, MeshHandoffOutcome::Applied);
        assert_eq!(disabled.snapshot(), NativeMeshTimingSnapshot::default());
        let mut enabled = NativeMeshTiming::new(true);
        enabled.settled(MeshPriority::Edit, None, NativeMeshDisposition::Skipped);
        enabled.uploaded(MeshPriority::Edit, None, origin, origin, MeshHandoffOutcome::Failed);
        assert_eq!(enabled.snapshot(), NativeMeshTimingSnapshot::default());
    }

    #[test]
    fn reversed_timestamps_count_invalid_phases_instead_of_zero_samples() {
        let origin = Instant::now();
        let mut trace = NativeMeshTiming::new(true);
        let timing = Some(NativeMeshResultTiming::new(
            at(origin, 37), at(origin, 13), Some(at(origin, 87)), at(origin, 56),
        ));
        trace.settled(MeshPriority::Edit, timing, NativeMeshDisposition::Built);
        trace.uploaded(MeshPriority::Edit, timing, at(origin, 31), at(origin, 19), MeshHandoffOutcome::Failed);
        let measured = trace.snapshot().by_priority[1];
        assert_eq!(measured.invalid_timestamps, 4);
        for phase in [measured.submit_to_receive, measured.mesh_compute,
            measured.completed_to_upload, measured.upload_cpu]
        {
            assert_eq!(phase, MeshPhaseMeasurement::default());
        }
        assert_eq!((measured.built, measured.failed), (1, 1));
    }
}
