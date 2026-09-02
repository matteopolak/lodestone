//! Hermetic unit tests for the chunk-batch size calculator.
//!
//! The aggregation is a rate the *server* consumes and clamps, so a wrong value
//! does not error — it silently makes chunk streaming pathologically slow or
//! bursty. These assert the running-average and clamp arithmetic against
//! hand-computed values so it cannot drift from vanilla's `ChunkBatchSizeCalculator`.

use lodestone_v26_2::chunk_batch::ChunkBatchSizeCalculator;

/// Vanilla seeds `aggregatedNanosPerChunk = 2_000_000` and weight 1, so the
/// initial desired rate is `7_000_000 / 2_000_000 = 3.5` chunks/tick.
#[test]
fn initial_desired_rate_matches_vanilla_seed() {
    let calc = ChunkBatchSizeCalculator::new();
    assert_eq!(calc.desired_chunks_per_tick(), 3.5);
}

#[test]
fn empty_batch_is_ignored() {
    let mut calc = ChunkBatchSizeCalculator::new();
    calc.on_batch_finished(0, 999_999_999.0);
    // Unchanged: still the seed rate.
    assert_eq!(calc.desired_chunks_per_tick(), 3.5);
}

#[test]
fn two_batches_fold_into_running_average() {
    let mut calc = ChunkBatchSizeCalculator::new();
    // Batch 1: 7 chunks in 7ms → 1_000_000 ns/chunk, within [666_667, 6_000_000].
    // aggregated = (2_000_000*1 + 1_000_000) / 2 = 1_500_000 → 7e6/1.5e6.
    calc.on_batch_finished(7, 7_000_000.0);
    assert!((calc.desired_chunks_per_tick() - 4.666_667).abs() < 1e-4);
    // Batch 2: 10 chunks in 5ms → 500_000 ns/chunk, clamped up to lower bound
    // 1_500_000/3 = 500_000 (no change). aggregated = (1_500_000*2 + 500_000)/3
    // = 3_500_000/3 → 7e6 / (3.5e6/3) = 6.0 exactly.
    calc.on_batch_finished(10, 5_000_000.0);
    assert!((calc.desired_chunks_per_tick() - 6.0).abs() < 1e-4);
}

#[test]
fn slow_batch_is_clamped_to_three_times_the_average() {
    let mut calc = ChunkBatchSizeCalculator::new();
    // 1 chunk in 100ms → 100_000_000 ns/chunk, clamped down to 3× the seed =
    // 6_000_000. aggregated = (2_000_000 + 6_000_000)/2 = 4_000_000 → 1.75.
    calc.on_batch_finished(1, 100_000_000.0);
    assert!((calc.desired_chunks_per_tick() - 1.75).abs() < 1e-4);
}

#[test]
fn fast_batch_is_clamped_to_a_third_of_the_average() {
    let mut calc = ChunkBatchSizeCalculator::new();
    // 1 chunk in 0ns → clamped up to seed/3 = 666_666.67. aggregated =
    // (2_000_000 + 666_666.67)/2 = 1_333_333.33 → 7e6/1.333e6 = 5.25.
    calc.on_batch_finished(1, 0.0);
    assert!((calc.desired_chunks_per_tick() - 5.25).abs() < 1e-4);
}
