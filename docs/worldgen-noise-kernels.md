# World-generation noise kernels

## What it is

The numeric world-generation core samples improved noise through an eight-lane
`std::simd` kernel. This document records the parity boundary and the small
layout control used to measure changes without conflating kernel cost with the
rest of production column generation.

## How it works

`ImprovedNoise::sample_and_lerp` keeps permutation-table traversal scalar because
each lookup depends on the previous lookup. It evaluates the eight gradient
dot products in independent lanes, then applies the existing x, y, and z lerp
tree in its original order. The lanes are laid out as the four x=0 corners
followed by the four x=1 corners; that makes the first reduction contiguous and
removes two de-interleaving shuffles on arm64. No horizontal reduction, fused
multiply-add, or gradient-component shortcut is allowed: those can change
rounding, NaN payload/sign, or signed-zero bits.

`crates/lodestone-worldgen-core/examples/noise_kernel_probe.rs` is the isolated
release control. It uses 1,048,576 deterministic fractional coordinates shaped
like 4×8 field-cell batches, repeats each arm seven times, and prints an FNV
digest of every result. The committed layout measured 22.568, 40.839, 80.488,
and 159.722 ns/sample for 2, 4, 8, and 16 active octaves in one baseline run;
the grouped-lane arm measured 23.115, 41.189, 78.550, and 156.352 ns/sample in
its first repeat, with the same digest for every octave count. Across repeated
runs the stable 8- and 16-octave medians improved by roughly 2–3%; the absolute
wall time is machine-load dependent, so the digest and instruction shape are
the durable controls.

## How to change it

Change only `noise/improved.rs` for the kernel and rerun the focused improved
tests plus `cargo run --release -p lodestone-worldgen-core --example
noise_kernel_probe`. Compare medians from separate foreground processes. The
tests compare raw `to_bits()` results against an independently written scalar
transcription, including signed-zero and scaled-entry controls; the JVM-backed
noise parity binary remains the authority for bundled fixtures.

Do not batch the octave accumulation in `PerlinNoise::get_value`: its order is
observable. A caller-facing position batch is a separate API decision and must
prove production consumption before it is counted as a performance result.

## Conservative terrain bounds

The production pre-corner density proof uses immutable scalar certificates alongside the exact
samplers. `BlendedNoise::conservative_overworld_bound` accepts only the stock scales, factors,
smear multiplier, complete reverse octave factors and finite octave offsets. Each gradient has
two nonzero coefficients of magnitude one. With smear `s`, the adjusted local Y coordinate is
bounded below by approximately `-epsilon*s`, where `epsilon=f64::from(1e-7_f32)`; the fade still
uses the original Y coordinate. A universal assumption that noise lies in `[-1,1]` is invalid.

For stock limit smear `684.412`, the real-arithmetic absolute bound after the reverse sum and
division by `65536` is `(2*65535 + 16*epsilon*684.412)/65536`, approximately `1.9999694992`.
The admitted scales keep wrapping and floor casts in range. A per-octave allowance below `2.0002`
covers floating-point arithmetic, and the returned `2.001` bound leaves further slack. Main noise
selects a clamped blend of finite limit stacks and cannot enlarge that bound.

Ordinary normal-noise certificates use the actual active amplitude/value-factor sums of both
stacks and their normalization multiplier. Unsupported factors, offsets, or nonfinite sums
decline the certificate. The entrance matcher requires a result below `3.999` for each of its
three relevant noises. Its finite first arm and bounded clamp ensure a finite final entrance
even if the clamp input is NaN: `f64::min` selects the finite first arm. Preserve that operator
semantics when extending the matcher. These bounds are never substituted for exact noise values
and never published to sampling caches. The owning flow is documented in
[compiled point density evaluation](./worldgen-point-density.md).

## End island geometric rejection

`EndIslandNoise::height_value` bounds a candidate before sampling simplex noise.
Each local coordinate has magnitude at most 25, so its squared distance `d2`
is an exactly represented integer at most 1250. Every slope is at least 9.
The candidate cannot raise current height `h` when
`81*d2 > (100-h+1/1024)^2`. This uses `f64` for the inequality and no square
root; admitted candidates keep the original `f32` evaluation and x-major order.

Finite running heights lie in `[-100,80]`, so the squared reach is positive.
The narrowed square root is below 36 with absolute error at most `2^-19`.
Multiplying by the minimum slope adds at most `2^-16` rounding error, and
subtracting from 100 adds at most `2^-17`. Their total height error is below
`9*2^-19 + 2^-16 + 2^-17 < 1/1024`. Larger slopes cannot increase a contribution
because multiplication, subtraction, and clamping are monotone. The allowance
also covers the much smaller `f64` inequality error. Rejection requires finite
`h`; strict comparison preserves ties. An initial height of exactly 80 returns
immediately because every candidate is clamped to at most 80. The wrapped radial
square and truncating coordinate divisions are unchanged.

Change `candidate_cannot_raise` in `noise/end_islands.rs` only with a corresponding
rounding proof. Its arithmetic control uses a 3-4-5 triangle: minimum slope 9
contributes 55, while an incorrect slope-22 bound discards the winning height.
The counted scalar-reference control checks plateau and mixed-sign residues;
the sampled comparison covers all nine residue pairs and overflow boundaries.
Run `cargo test -p lodestone-worldgen-core geometric_rejection --no-fail-fast`.
No production generator calls this kernel: the 26.3 engine samples End islands itself
(`engine::release26_3::sampler`).
This path introduces no cache, dependency, or configuration flag.

## Configuration

The kernel requires the workspace's pinned nightly compiler and the
`portable_simd` feature enabled at the `lodestone-worldgen-core` crate root.
The probe's `SAMPLES` and `ROUNDS` constants control measurement duration only;
they do not affect production behavior. `gen-counters` is intentionally off for
timings because its atomics perturb the hot path.

## Dependencies

The kernel depends only on the core crate's math helpers, `RandomSource`, and
the standard library's portable SIMD implementation. The probe depends on the
core crate's `PerlinNoise`, `NormalNoise`, and `XoroshiroRandomSource` types.
