//! Worldgen's benchmark recorder wrapper.
//!
//! The JSONL writer lives in `lodestone-testsupport`; this module retains the
//! worldgen-specific counter policy so counter builds cannot poison timing or
//! work-performed measurements.

pub use lodestone_testsupport::bench_record::Record;

/// Units whose values are structural and therefore remain comparable when the
/// generation counters are enabled. The closed list makes a new unit refuse by
/// default instead of silently recording a counters-inflated value.
const COUNTER_SAFE_UNITS: &[&str] = &["%", "x", "calls", "allocs", "draws", "bytes"];

fn timing_is_poisoned_by_counters(unit: &str, counters_enabled: bool) -> bool {
    counters_enabled && !COUNTER_SAFE_UNITS.contains(&unit)
}

/// Applies worldgen's counters guard, then delegates JSONL recording to the
/// shared native-only benchmark recorder.
pub fn record(rec: Record<'_>) {
    if timing_is_poisoned_by_counters(rec.unit, lodestone_worldgen::counters::enabled()) {
        eprintln!(
            "[bench-support] REFUSING to record {:?} ({} {}): this build has \
             `--features gen-counters`, which inflates a burst by roughly 3×, so the \
             number is not comparable to a clean run. Counter runs and timing runs must \
             be separate runs. Re-run without `--features gen-counters` for timings.",
            rec.metric, rec.value, rec.unit
        );
        return;
    }
    lodestone_testsupport::bench_record::record(rec);
}

#[cfg(test)]
mod tests {
    #[test]
    fn counter_guard_fails_closed_for_unlisted_units() {
        assert!(super::timing_is_poisoned_by_counters("instructions", true));
        assert!(super::timing_is_poisoned_by_counters("future-unit", true));
        assert!(!super::timing_is_poisoned_by_counters("calls", true));
        assert!(!super::timing_is_poisoned_by_counters("future-unit", false));
    }
}
