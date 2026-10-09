# World-generation noise kernels

## What it is

The numeric world-generation core's `f64` improved noise samples its eight corner gradients through an eight-lane `std::simd` kernel. Its callers are the vegetation engine's noise-driven block providers; the 26.3 engine samples its own noise.

## How it works

`ImprovedNoise::sample_and_lerp` keeps permutation-table traversal scalar, since each lookup depends on the previous one. It evaluates the eight gradient dot products in lanes, then applies the original x, y, z lerp tree in order.

Lanes are the four x=0 corners followed by the four x=1 corners, which makes the first reduction contiguous. No horizontal reduction, fused multiply-add or gradient shortcut is allowed: each can change rounding, NaN payload or signed-zero bits.

## How to change it

- Edit only `noise/improved.rs`, then rerun the improved-noise tests. They compare raw `to_bits()` against an independent scalar transcription; the JVM-backed noise parity binary is the authority for bundled fixtures.
- Do not batch the octave accumulation in `PerlinNoise::get_value`: its order is observable.
- A position-batch API is a separate decision and needs a production consumer before it counts as a speedup.

## Configuration

Needs the workspace's pinned nightly and the `portable_simd` feature at the `lodestone-worldgen-core` crate root. Leave `gen-counters` off for timings (its atomics perturb the hot path); `tests/simd_kernel_counter.rs` is the counted control.

## Dependencies

The core crate's math helpers, `RandomSource`, and portable SIMD from the standard library.
