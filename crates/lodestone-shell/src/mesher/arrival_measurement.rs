use std::collections::HashMap;
use std::time::Duration;

use super::measurement::MeshPhaseMeasurement;
use crate::platform::Instant;

const MAX_ARRIVAL_LIFETIMES: usize = 512;
type ColumnCoord = (i32, i32);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ArrivalMeasurementSnapshot {
    pub observed: u64,
    pub replacements: u64,
    pub overflow: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub eligible_without_observation: u64,
    pub admitted_without_observation: u64,
    pub admitted_without_eligibility: u64,
    pub forgotten_without_observation: u64,
    pub invalid_timestamps: u64,
    pub pending: usize,
    pub pending_eligibility: usize,
    pub observed_to_eligible: MeshPhaseMeasurement,
    pub eligible_to_admitted: MeshPhaseMeasurement,
}

#[derive(Debug, Clone, Copy)]
enum ArrivalLifetime {
    Waiting { observed_at: Instant },
    Eligible { observed_at: Instant, eligible_at: Instant },
}

#[derive(Debug, Default)]
struct ArrivalState {
    lifetimes: HashMap<ColumnCoord, ArrivalLifetime>,
    totals: ArrivalMeasurementSnapshot,
}

/// Opt-in shell arrival-to-admission timing, independent of section mesh queues.
#[derive(Debug, Default)]
pub(super) struct ArrivalMeasurement {
    state: Option<ArrivalState>,
}

impl ArrivalMeasurement {
    pub(super) fn new(enabled: bool) -> Self {
        Self { state: enabled.then(ArrivalState::default) }
    }

    pub(super) fn enabled(&self) -> bool {
        self.state.is_some()
    }

    pub(super) fn contains(&self, coord: ColumnCoord) -> bool {
        self.state.as_ref().is_some_and(|state| state.lifetimes.contains_key(&coord))
    }

    pub(super) fn observed(&mut self, coord: ColumnCoord, now: Instant) {
        let Some(state) = &mut self.state else { return };
        state.totals.observed = state.totals.observed.saturating_add(1);
        if let Some(lifetime) = state.lifetimes.get_mut(&coord) {
            *lifetime = ArrivalLifetime::Waiting { observed_at: now };
            state.totals.replacements = state.totals.replacements.saturating_add(1);
        } else if state.lifetimes.len() < MAX_ARRIVAL_LIFETIMES {
            state.lifetimes.insert(coord, ArrivalLifetime::Waiting { observed_at: now });
        } else {
            state.totals.overflow = state.totals.overflow.saturating_add(1);
        }
    }

    pub(super) fn eligible(&mut self, coord: ColumnCoord, now: Instant) {
        let Some(state) = &mut self.state else { return };
        match state.lifetimes.get_mut(&coord) {
            Some(lifetime) => {
                if let ArrivalLifetime::Waiting { observed_at } = *lifetime {
                    *lifetime = ArrivalLifetime::Eligible { observed_at, eligible_at: now };
                }
            }
            None => {
                state.totals.eligible_without_observation =
                    state.totals.eligible_without_observation.saturating_add(1);
            }
        }
    }

    pub(super) fn admitted(&mut self, coord: ColumnCoord, now: Instant) {
        let Some(state) = &mut self.state else { return };
        match state.lifetimes.remove(&coord) {
            Some(ArrivalLifetime::Eligible { observed_at, eligible_at }) => {
                let waits = eligible_at.checked_duration_since(observed_at)
                    .zip(now.checked_duration_since(eligible_at));
                if let Some((eligibility, admission)) = waits {
                    record(&mut state.totals.observed_to_eligible, eligibility);
                    record(&mut state.totals.eligible_to_admitted, admission);
                    state.totals.completed = state.totals.completed.saturating_add(1);
                } else {
                    state.totals.invalid_timestamps = state.totals.invalid_timestamps.saturating_add(1);
                }
            }
            Some(ArrivalLifetime::Waiting { .. }) => {
                state.totals.admitted_without_eligibility =
                    state.totals.admitted_without_eligibility.saturating_add(1);
            }
            None => {
                state.totals.admitted_without_observation =
                    state.totals.admitted_without_observation.saturating_add(1);
            }
        }
    }

    pub(super) fn forget(&mut self, coord: ColumnCoord) {
        let Some(state) = &mut self.state else { return };
        if state.lifetimes.remove(&coord).is_some() {
            state.totals.cancelled = state.totals.cancelled.saturating_add(1);
        } else {
            state.totals.forgotten_without_observation =
                state.totals.forgotten_without_observation.saturating_add(1);
        }
    }

    pub(super) fn clear(&mut self) {
        let Some(state) = &mut self.state else { return };
        state.totals.cancelled = state.totals.cancelled.saturating_add(state.lifetimes.len() as u64);
        state.lifetimes.clear();
    }

    pub(super) fn snapshot(&self) -> ArrivalMeasurementSnapshot {
        let Some(state) = &self.state else { return ArrivalMeasurementSnapshot::default() };
        ArrivalMeasurementSnapshot {
            pending: state.lifetimes.len(),
            pending_eligibility: state.lifetimes.values()
                .filter(|lifetime| matches!(lifetime, ArrivalLifetime::Waiting { .. })).count(),
            ..state.totals
        }
    }

    pub(super) fn oldest_waits(&self, now: Instant) -> (Option<Duration>, Option<Duration>) {
        let Some(state) = &self.state else { return (None, None) };
        let mut eligibility = None;
        let mut admission = None;
        for lifetime in state.lifetimes.values() {
            let (start, oldest) = match lifetime {
                ArrivalLifetime::Waiting { observed_at } => (*observed_at, &mut eligibility),
                ArrivalLifetime::Eligible { eligible_at, .. } => (*eligible_at, &mut admission),
            };
            if let Some(elapsed) = now.checked_duration_since(start) {
                *oldest = Some(oldest.map_or(elapsed, |previous: Duration| previous.max(elapsed)));
            }
        }
        (eligibility, admission)
    }
}

fn record(totals: &mut MeshPhaseMeasurement, elapsed: Duration) {
    let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
    totals.calls = totals.calls.saturating_add(1);
    totals.total_ns = totals.total_ns.saturating_add(ns);
    totals.max_ns = totals.max_ns.max(ns);
}

impl std::fmt::Display for ArrivalMeasurementSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f,
            "mesh arrival: observed={} completed={} replaced={} cancelled={} overflow={} pending={} waiting_eligibility={} missing_eligibility={} invalid_times={} eligibility_calls={} eligibility_total_ms={:.3} eligibility_max_ms={:.3} admission_calls={} admission_total_ms={:.3} admission_max_ms={:.3}",
            self.observed, self.completed, self.replacements, self.cancelled, self.overflow,
            self.pending, self.pending_eligibility, self.admitted_without_eligibility,
            self.invalid_timestamps, self.observed_to_eligible.calls,
            self.observed_to_eligible.total_ns as f64 / 1e6,
            self.observed_to_eligible.max_ns as f64 / 1e6,
            self.eligible_to_admitted.calls, self.eligible_to_admitted.total_ns as f64 / 1e6,
            self.eligible_to_admitted.max_ns as f64 / 1e6,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(origin: Instant, millis: u64) -> Instant {
        origin + Duration::from_millis(millis)
    }

    #[test]
    fn arrival_measurement_separates_held_halo_from_held_admission() {
        let origin = Instant::now();
        let mut halo = ArrivalMeasurement::new(true);
        halo.observed((4, -7), at(origin, 13));
        halo.eligible((4, -7), at(origin, 114));
        halo.admitted((4, -7), at(origin, 121));
        let halo = halo.snapshot();
        assert_eq!(halo.observed_to_eligible, MeshPhaseMeasurement {
            calls: 1, total_ns: 101_000_000, max_ns: 101_000_000,
        });
        assert_eq!(halo.eligible_to_admitted, MeshPhaseMeasurement {
            calls: 1, total_ns: 7_000_000, max_ns: 7_000_000,
        });

        let mut admission = ArrivalMeasurement::new(true);
        admission.observed((4, -7), at(origin, 13));
        admission.eligible((4, -7), at(origin, 20));
        admission.admitted((4, -7), at(origin, 121));
        let admission = admission.snapshot();
        assert_eq!(admission.observed_to_eligible, MeshPhaseMeasurement {
            calls: 1, total_ns: 7_000_000, max_ns: 7_000_000,
        });
        assert_eq!(admission.eligible_to_admitted, MeshPhaseMeasurement {
            calls: 1, total_ns: 101_000_000, max_ns: 101_000_000,
        });
        assert_eq!((halo.completed, admission.completed), (1, 1));
        assert_eq!((halo.pending, admission.pending), (0, 0));
    }

    #[test]
    fn arrival_measurement_preserves_first_eligibility_and_accumulates_exact_totals() {
        let origin = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        trace.observed((2, 3), at(origin, 11));
        trace.eligible((2, 3), at(origin, 28));
        trace.eligible((2, 3), at(origin, 91));
        trace.admitted((2, 3), at(origin, 99));
        trace.observed((-2, 3), at(origin, 101));
        trace.eligible((-2, 3), at(origin, 124));
        trace.admitted((-2, 3), at(origin, 153));
        let totals = trace.snapshot();
        assert_eq!(totals.observed_to_eligible, MeshPhaseMeasurement {
            calls: 2, total_ns: 40_000_000, max_ns: 23_000_000,
        });
        assert_eq!(totals.eligible_to_admitted, MeshPhaseMeasurement {
            calls: 2, total_ns: 100_000_000, max_ns: 71_000_000,
        });
        assert_eq!((totals.observed, totals.completed), (2, 2));
    }

    #[test]
    fn arrival_measurement_replacement_discards_old_lifetime_and_unload_invalidates() {
        let origin = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        trace.observed((8, 9), at(origin, 5));
        trace.eligible((8, 9), at(origin, 6));
        trace.observed((8, 9), at(origin, 31));
        assert_eq!(trace.snapshot().pending_eligibility, 1);
        trace.eligible((8, 9), at(origin, 44));
        trace.admitted((8, 9), at(origin, 63));
        trace.observed((8, 9), at(origin, 71));
        trace.eligible((8, 9), at(origin, 73));
        trace.forget((8, 9));
        trace.admitted((8, 9), at(origin, 101));
        let totals = trace.snapshot();
        assert_eq!((totals.observed, totals.replacements, totals.cancelled), (3, 1, 1));
        assert_eq!((totals.completed, totals.admitted_without_observation), (1, 1));
        assert_eq!(totals.observed_to_eligible.total_ns, 13_000_000);
        assert_eq!(totals.eligible_to_admitted.total_ns, 19_000_000);
        assert_eq!(totals.pending, 0);
    }

    #[test]
    fn arrival_measurement_missing_stages_are_not_zero_duration_samples() {
        let now = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        trace.eligible((1, 1), now);
        trace.admitted((1, 1), now);
        trace.forget((1, 1));
        trace.observed((2, 2), now);
        trace.admitted((2, 2), now);
        trace.observed((3, 3), now);
        trace.observed((4, 4), now);
        trace.eligible((4, 4), now);
        let pending = trace.snapshot();
        assert_eq!((pending.pending, pending.pending_eligibility), (2, 1));
        trace.clear();
        trace.admitted((4, 4), now);
        let totals = trace.snapshot();
        assert_eq!(totals.eligible_without_observation, 1);
        assert_eq!(totals.admitted_without_observation, 2);
        assert_eq!(totals.forgotten_without_observation, 1);
        assert_eq!(totals.admitted_without_eligibility, 1);
        assert_eq!((totals.cancelled, totals.pending, totals.completed), (2, 0, 0));
        assert_eq!(totals.observed_to_eligible, MeshPhaseMeasurement::default());
        assert_eq!(totals.eligible_to_admitted, MeshPhaseMeasurement::default());
    }

    #[test]
    fn arrival_measurement_bound_rejects_overflow_without_evicting_existing_lifetimes() {
        let origin = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        for cx in 0..512 {
            trace.observed((cx, -1), origin);
        }
        trace.observed((512, -1), origin);
        trace.observed((0, -1), at(origin, 3));
        trace.eligible((0, -1), at(origin, 8));
        trace.admitted((0, -1), at(origin, 19));
        trace.admitted((512, -1), at(origin, 20));
        trace.observed((513, -1), at(origin, 21));
        let totals = trace.snapshot();
        assert_eq!((totals.observed, totals.pending), (515, 512));
        assert_eq!((totals.overflow, totals.replacements), (1, 1));
        assert_eq!((totals.completed, totals.admitted_without_observation), (1, 1));
        assert_eq!(totals.observed_to_eligible.total_ns, 5_000_000);
        assert_eq!(totals.eligible_to_admitted.total_ns, 11_000_000);
        trace.clear();
        assert_eq!((trace.snapshot().cancelled, trace.snapshot().pending), (512, 0));
    }

    #[test]
    fn arrival_measurement_disabled_has_no_state_or_counters() {
        let now = Instant::now();
        for mut trace in [ArrivalMeasurement::default(), ArrivalMeasurement::new(false)] {
            assert!(!trace.enabled());
            trace.observed((1, 2), now);
            trace.eligible((1, 2), now);
            trace.admitted((1, 2), now);
            trace.forget((1, 2));
            trace.clear();
            assert!(trace.state.is_none());
            assert_eq!(trace.snapshot(), ArrivalMeasurementSnapshot::default());
        }
    }

    #[test]
    fn arrival_measurement_reversed_timestamps_are_incomplete_not_saturated_to_zero() {
        let origin = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        trace.observed((1, 2), at(origin, 40));
        trace.eligible((1, 2), at(origin, 39));
        trace.admitted((1, 2), at(origin, 50));
        trace.observed((2, 3), at(origin, 60));
        trace.eligible((2, 3), at(origin, 65));
        trace.admitted((2, 3), at(origin, 64));
        let totals = trace.snapshot();
        assert_eq!((totals.invalid_timestamps, totals.completed, totals.pending), (2, 0, 0));
        assert_eq!(totals.observed_to_eligible.calls, 0);
        assert_eq!(totals.eligible_to_admitted.calls, 0);
    }

    #[test]
    fn arrival_measurement_oldest_waits_include_unfinished_work_only() {
        let origin = Instant::now();
        let mut trace = ArrivalMeasurement::new(true);
        trace.observed((1, 2), at(origin, 13));
        trace.observed((2, 3), at(origin, 19));
        assert!(trace.contains((1, 2)));
        trace.eligible((2, 3), at(origin, 37));
        assert_eq!(trace.oldest_waits(at(origin, 101)), (
            Some(Duration::from_millis(88)), Some(Duration::from_millis(64)),
        ));
        trace.admitted((2, 3), at(origin, 109));
        trace.forget((1, 2));
        assert!(!trace.contains((1, 2)));
        assert_eq!(trace.oldest_waits(at(origin, 121)), (None, None));
        assert_eq!(ArrivalMeasurement::new(false).oldest_waits(origin), (None, None));
    }
}
