# World-generation noise kernels

## What it is

The numeric world-generation core's `f64` improved noise samples its eight
corner gradients through an eight-lane `std::simd` kernel. Its callers are the
vegetation engine's noise-driven block providers; the 26.3 engine samples its
own noise. This document records the kernel's parity boundary.

## How it works

`ImprovedNoise::sample_and_lerp` keeps permutation-table traversal scalar because
each lookup depends on the previous lookup. It evaluates the eight gradient
dot products in independent lanes, then applies the existing x, y, and z lerp
tree in its original order. The lanes are laid out as the four x=0 corners
followed by the four x=1 corners; that makes the first reduction contiguous and
removes two de-interleaving shuffles on arm64. No horizontal reduction, fused
multiply-add, or gradient-component shortcut is allowed: those can change
rounding, NaN payload/sign, or signed-zero bits.

## How to change it

Change only `noise/improved.rs` for the kernel and rerun the focused improved
tests. The tests compare raw `to_bits()` results against an independently written scalar
transcription, including signed-zero and scaled-entry controls; the JVM-backed
noise parity binary remains the authority for bundled fixtures.

Do not batch the octave accumulation in `PerlinNoise::get_value`: its order is
observable. A caller-facing position batch is a separate API decision and must
prove production consumption before it is counted as a performance result.

## Configuration

The kernel requires the workspace's pinned nightly compiler and the
`portable_simd` feature enabled at the `lodestone-worldgen-core` crate root.
`gen-counters` is intentionally off for timings because its atomics perturb the
hot path; `tests/simd_kernel_counter.rs` is the counted control.

## Dependencies

The kernel depends only on the core crate's math helpers, `RandomSource`, and
the standard library's portable SIMD implementation.
