//! Version-free Minecraft Java Edition world-generation engine.
//!
//! This crate contains **no version-specific data**. It provides the shared,
//! hand-written machinery that vanilla worldgen is built on — seeded RNG, noise,
//! and (progressively) the density-function interpreter — parameterised by data
//! that a version crate supplies (noise settings, density functions, biome
//! definitions). Dropping a version drops its data, never this engine (plan §3).
//!
//! # Parity discipline
//!
//! Every primitive here is proven bit-for-bit against a real JVM. The oracle in
//! `scripts/worldgen-oracle/` calls the **actual 26.2 game classes' public
//! APIs** (the [`ShapeOracle`] pattern) to dump ground-truth values; the Rust
//! here is written originally from the documented algorithms (never
//! transliterated, plan §11) and diffed element-wise against those dumps. A
//! failing test names the exact key that diverged.
//!
//! [`ShapeOracle`]: https://example.invalid/ (see crates/lodestone-physics/oracle-java)
//!
//! # Layout
//!
//! * [`rng`] — `RandomSource` implementations: the legacy `java.util.Random`
//!   LCG and the 1.18+ `XoroshiroRandomSource`, plus positional factories and
//!   the `WorldgenRandom` seed derivations.
//! * [`hash`] — the standalone hashing (MD5, Java `String::hashCode`) that
//!   worldgen seeding depends on.

pub mod aquifer;
pub mod biome;
pub mod carver;
pub mod compose;
pub mod counters;
pub mod dense_grid;
pub mod density;
pub mod feature;
pub mod hash;
pub mod interner;
pub mod math;
pub mod noise;
pub mod overworld;
pub mod rng;
pub mod surface;

pub use noise::{ImprovedNoise, NormalNoise, PerlinNoise};
pub use rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource, WorldgenRandom,
    XoroshiroRandomSource,
};
