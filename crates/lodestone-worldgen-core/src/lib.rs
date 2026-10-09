//! Numeric core of world generation: hashing, seeded RNG, the `f64` noise
//! stack, the shared math helpers, the structural counters, and the 26.3
//! terrain engine ([`engine::release26_3`]).
//!
//! [`lodestone-worldgen`] depends on this crate and re-exports its modules
//! under its own paths, so `lodestone_worldgen::rng` and
//! `lodestone_worldgen_core::rng` are the same module. Structure placement,
//! features and the per-dimension driver live there.
//!
//! [`lodestone-worldgen`]: https://example.invalid/ (see crates/lodestone-worldgen)
//!
//! # Why this is a separate crate
//!
//! Scheduling: the feature and structure layer is the most-edited worldgen
//! code, and keeping the numeric kernels in a leaf crate means editing it does
//! not rebuild them. The leaf has no code edges back into `lodestone-worldgen`;
//! JSON parsing, SHA-256 and portable SIMD are its only external dependencies.
//!
//! # Parity discipline
//!
//! Every primitive here is proven bit-for-bit against a real JVM: oracle
//! programs call the reference's own public APIs to dump ground truth, and the
//! Rust is written from the documented algorithms and diffed element-wise
//! against those dumps. The RNG, math and noise dumps are checked by
//! `lodestone-worldgen/tests/{rng,mth,noise}_parity.rs`; the 26.3 engine's are
//! the `release26_3_*` tests in this crate.

// Improved noise uses one portable-SIMD implementation, gated bit-exact against
// the JVM fixtures.
#![feature(portable_simd)]

pub mod counters;
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
