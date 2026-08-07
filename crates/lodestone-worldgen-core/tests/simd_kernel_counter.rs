//! Unit 5's island check: proof that the vectorised noise kernel is the path
//! production actually takes, expressed as a **prediction** rather than a smoke
//! test.
//!
//! Its own binary, not a unit test in the lib, because `counters` is
//! process-global: other tests in the lib binary instantiate `NormalNoise` and
//! would inflate a before/after delta measured alongside them. That is the same
//! reason `lodestone-worldgen/tests/engine_counters.rs` is a separate binary.
//!
//! Without `--features gen-counters` every hook is an empty `#[inline(always)]`
//! function, so the assertions below are compiled out and the file reports a
//! deliberate skip rather than a silent pass — see
//! [`counters_are_compiled_out_without_the_feature`].
//!
//! What this file does **not** prove: that the lanes stayed vectorised. A counter
//! cannot see LLVM scalarising a `Simd` op; that question is answered by
//! disassembly, recorded in `docs/worldgen-simd-kernels.md`.

use lodestone_worldgen_core::counters;
use lodestone_worldgen_core::noise::perlin::PerlinNoise;
use lodestone_worldgen_core::rng::XoroshiroRandomSource;

/// One `ImprovedNoise` sample is exactly one eight-lane gradient batch, so a
/// `PerlinNoise` stack with `k` non-zero amplitudes costs exactly `k` batches per
/// `get_value`. The expected numbers below are derived from the *amplitude list*
/// — outside the kernel — not read back from a previous run.
#[cfg(feature = "gen-counters")]
#[test]
fn batch_count_equals_the_octave_count_the_amplitudes_declare() {
    let mut rng = XoroshiroRandomSource::new(42);
    let four = PerlinNoise::create(&mut rng, -4, &[1.0, 1.0, 1.0, 1.0]);

    // A zero amplitude leaves its octave un-instantiated (`None`), so it must
    // cost nothing. This is the discriminating case: a kernel counted per
    // *octave slot* rather than per *sample* would read 4 here, not 2.
    let mut rng2 = XoroshiroRandomSource::new(42);
    let two_of_four = PerlinNoise::create(&mut rng2, -4, &[1.0, 0.0, 1.0, 0.0]);

    counters::reset();
    let base = counters::snapshot().noise_corner_batches;
    assert_eq!(base, 0, "reset() left the batch counter non-zero");

    let a = four.get_value(12.5, 33.25, -7.75);
    let after_four = counters::snapshot().noise_corner_batches;
    assert_eq!(
        after_four, 4,
        "four unit amplitudes must cost exactly four eight-lane batches; 0 would \
         mean the SIMD kernel is not on the production path at all"
    );

    let b = two_of_four.get_value(12.5, 33.25, -7.75);
    let after_two = counters::snapshot().noise_corner_batches;
    assert_eq!(
        after_two - after_four,
        2,
        "two non-zero amplitudes of four must cost exactly two batches; reading 4 \
         would mean the counter is per octave slot rather than per sample"
    );

    // Non-vacuity: the samples have to be real values, or the counts above could
    // be coming from a kernel that returned early.
    assert!(a.is_finite() && b.is_finite(), "noise samples were not finite");
    assert_ne!(
        a.to_bits(),
        b.to_bits(),
        "the four-octave and two-octave stacks produced identical values, so this \
         fixture is not distinguishing them"
    );
}

/// The default build must carry no counter code. Asserting the *absence* needs
/// evidence the detector works, so this also drives the kernel: the counter is
/// zero **after** work that would have incremented a live one.
#[cfg(not(feature = "gen-counters"))]
#[test]
fn counters_are_compiled_out_without_the_feature() {
    let mut rng = XoroshiroRandomSource::new(42);
    let noise = PerlinNoise::create(&mut rng, -4, &[1.0, 1.0, 1.0, 1.0]);
    let v = noise.get_value(12.5, 33.25, -7.75);
    assert!(v.is_finite(), "the kernel did not produce a value");
    assert_eq!(
        counters::snapshot().noise_corner_batches,
        0,
        "the batch counter is live without the gen-counters feature; every hook is \
         supposed to be an empty inline function in a shipped build"
    );
}
