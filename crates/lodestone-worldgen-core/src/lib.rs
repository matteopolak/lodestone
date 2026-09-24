//! Numeric core of the version-free world-generation engine.
//!
//! The arithmetic half of [`lodestone-worldgen`]: hashing, seeded RNG, noise
//! synthesis, and the density-function interpreter, plus the structural counters
//! that instrument them. Everything above this line — aquifers, carvers,
//! surface rules, features, biome selection and the composed overworld driver —
//! stays in `lodestone-worldgen`, which depends on this crate and re-exports
//! every module below under its own path, so `lodestone_worldgen::density` and
//! `lodestone_worldgen_core::density` are the same module.
//!
//! [`lodestone-worldgen`]: https://example.invalid/ (see crates/lodestone-worldgen)
//!
//! # Why this is a separate crate
//!
//! Unit 16 of [the worldgen rewrite plan] extracted it, and the reason is
//! scheduling rather than aesthetics — see
//! `docs/worldgen-module-layout.md` for the measurement:
//!
//! * The feature layer (`feature/`, `overworld/`) is the most-edited code in the
//!   rewrite and no longer rebuilds these 4,372 lines on every touch.
//! * The numeric kernels are the exact surface Unit 5 SIMDs, so
//!   `#![feature(portable_simd)]` can land on a crate that does not drag the
//!   feature layer in with it.
//! * Unit 4's new density engine is born here rather than written in
//!   `lodestone-worldgen` and moved afterwards.
//!
//! [the worldgen rewrite plan]: https://example.invalid/ (see docs/plans/worldgen-rewrite.md)
//!
//! # The boundary, and why `counters` is inside it
//!
//! This is a **leaf**: its only non-`std` dependency is `serde_json`, and it has
//! zero code edges back into `lodestone-worldgen`. `counters` belongs here and
//! not in the parent for a measured reason — `density/chunk.rs` and `rng` call
//! into it (8 call sites), and it depends back on
//! [`density::Density::KIND_COUNT`] for two array sizes and a loop bound. That
//! cycle cannot be cut by a crate boundary, so the closed set is these six
//! modules together. Extracting the five without `counters` would put those 8
//! call sites in this crate pointing back at the parent.
//!
//! # Parity discipline
//!
//! Every primitive here is proven bit-for-bit against a real JVM. The oracle in
//! `scripts/worldgen-oracle/` calls the **actual 26.2 game classes' public
//! APIs** to dump ground-truth values; the Rust here is written originally from
//! the documented algorithms (never transliterated, plan §11) and diffed
//! element-wise against those dumps. The fixture-backed `*_parity` binaries
//! (`mth`, `rng`, `noise`, `density`, …) live in `lodestone-worldgen/tests/`
//! alongside their JVM dumps and drive these modules through the parent's
//! re-exports, so they were not moved and their fixture paths are unchanged.

// Unit 5: `std::simd` in the noise kernels. The workspace is pinned to nightly
// (see `rust-toolchain.toml`) specifically so this needs **no** `#[cfg]`-selected
// scalar fallback — a dual path is two worlds from one seed waiting to happen,
// each arm individually "correct" and invisible to a single-arm test run. The one
// vectorised kernel is `noise::improved::ImprovedNoise::sample_and_lerp`, gated
// bit-exact against the JVM fixtures like everything else.
#![feature(portable_simd)]

pub mod counters;
pub mod density;
pub mod engine;
pub mod hash;
pub mod math;
pub mod noise;
pub mod rng;

pub use noise::{ImprovedNoise, NormalNoise, PerlinNoise};
pub use rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource, WorldgenRandom,
    XoroshiroRandomSource,
};
