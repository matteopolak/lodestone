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
