use std::time::Duration;

use super::Meshed;

/// The coalesced request consumed by the browser's late capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRequestCause {
    Column,
    Section,
    Light,
    Explicit,
}

impl MeshRequestCause {
    pub const ALL: [Self; 4] = [Self::Column, Self::Section, Self::Light, Self::Explicit];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Column => 0,
            Self::Section => 1,
            Self::Light => 2,
            Self::Explicit => 3,
        }
    }
}

/// The actual renderer result, not a prediction from CPU mesh contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshHandoffOutcome {
    Applied,
    Unchanged,
    Failed,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeshPhaseMeasurement {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
}

impl MeshPhaseMeasurement {
    pub(super) fn record(&mut self, elapsed: Duration) {
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.calls = self.calls.saturating_add(1);
        self.total_ns = self.total_ns.saturating_add(ns);
        self.max_ns = self.max_ns.max(ns);
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn merge(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.total_ns = self.total_ns.saturating_add(other.total_ns);
        self.max_ns = self.max_ns.max(other.max_ns);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeshLightReadMeasurement {
    pub value_buckets: [u64; 5],
    pub unique_cells: u64,
    pub reads: u64,
    pub out_of_domain_reads: u64,
}

impl MeshLightReadMeasurement {
    pub(super) fn record(&mut self, summary: super::light_reads::LightReadSummary) {
        let bucket = match summary.distinct_values {
            0 => 0,
            1 => 1,
            2 => 2,
            3..=4 => 3,
            _ => 4,
        };
        self.value_buckets[bucket] = self.value_buckets[bucket].saturating_add(1);
        self.unique_cells = self.unique_cells.saturating_add(u64::from(summary.unique_cells));
        self.reads = self.reads.saturating_add(summary.reads);
        self.out_of_domain_reads = self.out_of_domain_reads.saturating_add(summary.out_of_domain_reads);
    }

    #[cfg(any(target_arch = "wasm32", test))]
    fn merge(&mut self, other: Self) {
        for (total, count) in self.value_buckets.iter_mut().zip(other.value_buckets) {
            *total = total.saturating_add(count);
        }
        self.unique_cells = self.unique_cells.saturating_add(other.unique_cells);
        self.reads = self.reads.saturating_add(other.reads);
        self.out_of_domain_reads = self.out_of_domain_reads.saturating_add(other.out_of_domain_reads);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeshCauseMeasurement {
    pub light_inputs: MeshPhaseMeasurement,
    pub capture: MeshPhaseMeasurement,
    pub models: MeshPhaseMeasurement,
    pub fluids: MeshPhaseMeasurement,
    pub visibility: MeshPhaseMeasurement,
    pub packed: MeshPhaseMeasurement,
    pub fingerprint: MeshPhaseMeasurement,
    pub built: u64,
    pub applied: u64,
    pub unchanged: u64,
    pub failed: u64,
    pub light_reads: MeshLightReadMeasurement,
}

/// Session totals with fixed storage and no retained snapshots or geometry.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MeshMeasurementSnapshot {
    pub by_cause: [MeshCauseMeasurement; 4],
}

impl MeshMeasurementSnapshot {
    #[cfg(any(target_arch = "wasm32", test))]
    pub(super) fn capture(&mut self, cause: MeshRequestCause, elapsed: Duration) {
        self.by_cause[cause.index()].capture.record(elapsed);
    }

    #[cfg(any(target_arch = "wasm32", test))]
    pub(super) fn built(&mut self, cause: MeshRequestCause, passes: MeshCauseMeasurement) {
        let totals = &mut self.by_cause[cause.index()];
        totals.built = totals.built.saturating_add(1);
        totals.models.merge(passes.models);
        totals.fluids.merge(passes.fluids);
        totals.visibility.merge(passes.visibility);
        totals.packed.merge(passes.packed);
        totals.fingerprint.merge(passes.fingerprint);
        totals.light_inputs.merge(passes.light_inputs);
        totals.light_reads.merge(passes.light_reads);
    }

    pub(super) fn handoff(&mut self, meshed: &Meshed, outcome: MeshHandoffOutcome) {
        let Some(cause) = meshed.measurement_cause else { return };
        let totals = &mut self.by_cause[cause.index()];
        let count = match outcome {
            MeshHandoffOutcome::Applied => &mut totals.applied,
            MeshHandoffOutcome::Unchanged => &mut totals.unchanged,
            MeshHandoffOutcome::Failed => &mut totals.failed,
        };
        *count = count.saturating_add(1);
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn enabled() -> bool {
    log::log_enabled!(target: "frame_profile", log::Level::Debug)
}

pub(super) fn phase<T>(
    enabled: bool,
    counter: &mut MeshPhaseMeasurement,
    work: impl FnOnce() -> T,
) -> T {
    let started = enabled.then(crate::platform::Instant::now);
    let result = work();
    if let Some(started) = started {
        counter.record(started.elapsed());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesher::{SectionGeometry, SectionKey};

    #[test]
    fn renderer_outcomes_are_separate_from_builds_and_keep_the_capture_cause() {
        let mut totals = MeshMeasurementSnapshot::default();
        let mut meshed = Meshed::new(
            SectionKey { cx: 2, cz: -3, si: 1, min_y: -64 },
            SectionGeometry::Packed(lodestone_render::Mesh::default()),
        );
        totals.handoff(&meshed, MeshHandoffOutcome::Applied);
        assert_eq!(totals, MeshMeasurementSnapshot::default());
        meshed.measurement_cause = Some(MeshRequestCause::Light);
        totals.capture(MeshRequestCause::Column, Duration::from_nanos(13));
        totals.built(MeshRequestCause::Light, MeshCauseMeasurement::default());
        totals.handoff(&meshed, MeshHandoffOutcome::Unchanged);
        totals.handoff(&meshed, MeshHandoffOutcome::Failed);
        totals.handoff(&meshed, MeshHandoffOutcome::Applied);
        let light = totals.by_cause[MeshRequestCause::Light.index()];
        assert_eq!((light.built, light.applied, light.unchanged, light.failed), (1, 1, 1, 1));
        let column = totals.by_cause[MeshRequestCause::Column.index()];
        assert_eq!((column.capture.calls, column.capture.total_ns, column.built), (1, 13, 0));
        assert_eq!(totals.by_cause[MeshRequestCause::Section.index()], MeshCauseMeasurement::default());
    }

    #[test]
    fn disabled_timing_still_executes_work_and_phase_totals_are_not_nested() {
        let mut phases = MeshCauseMeasurement::default();
        assert_eq!(phase(false, &mut phases.models, || 37), 37);
        assert_eq!(phases.models, MeshPhaseMeasurement::default());
        phases.models.record(Duration::from_nanos(7));
        phases.models.record(Duration::from_nanos(11));
        phases.fluids.record(Duration::from_nanos(29));
        phases.light_reads.record(super::super::light_reads::LightReadSummary {
            distinct_values: 2, unique_cells: 9, reads: 13, out_of_domain_reads: 1,
        });
        let mut totals = MeshMeasurementSnapshot::default();
        totals.built(MeshRequestCause::Section, phases);
        let measured = totals.by_cause[MeshRequestCause::Section.index()];
        assert_eq!(measured.models, MeshPhaseMeasurement { calls: 2, total_ns: 18, max_ns: 11 });
        assert_eq!(measured.fluids, MeshPhaseMeasurement { calls: 1, total_ns: 29, max_ns: 29 });
        assert_eq!(measured.visibility.calls, 0);
        assert_eq!(measured.light_reads.value_buckets, [0, 0, 1, 0, 0]);
        assert_eq!((measured.light_reads.unique_cells, measured.light_reads.reads,
            measured.light_reads.out_of_domain_reads), (9, 13, 1));
        // Fixed storage, nothing retained per mesh: four causes, each seven
        // three-word phase records, four counters, and the eight-word light-read
        // record. Any new field must be a deliberate change to this arithmetic.
        let per_cause = 7 * 3 * 8 + 4 * 8 + (5 + 3) * 8;
        assert_eq!(std::mem::size_of::<MeshMeasurementSnapshot>(), 4 * per_cause);
    }
}
