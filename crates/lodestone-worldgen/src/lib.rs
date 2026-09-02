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
//!
//! # The numeric core is a separate crate
//!
//! `counters`, `density`, `hash`, `math`, `noise` and `rng` live in
//! `lodestone-worldgen-core` (Unit 16 of the rewrite plan; see
//! `docs/worldgen-module-layout.md`) and are **re-exported below under their
//! original paths**, so `lodestone_worldgen::density::Resolver` and every other
//! path a caller already spells still resolves — the split moved no public path.
//! Add new numeric/kernel code there and new pipeline code here.

pub mod aquifer;
pub mod biome;
pub mod carver;
pub mod compose;
pub mod debug;
pub mod dense_grid;
pub mod end;
pub mod feature;
pub mod flat;
pub mod generator;
pub mod interner;
pub mod nether;
pub mod overworld;
pub mod profile;
pub mod spawn_stage;
pub mod spawners;
pub mod structure;
pub mod surface;
pub mod table_resolver;

/// The numeric core, re-exported so every pre-split path keeps resolving.
///
/// These are modules of `lodestone-worldgen-core`, not of this crate. Nothing
/// else in the workspace had to change: `crate::density::…` inside this crate
/// and `lodestone_worldgen::density::…` outside it both route through here.
pub use lodestone_worldgen_core::{counters, density, engine, hash, math, noise, rng};

pub use noise::{ImprovedNoise, NormalNoise, PerlinNoise};
pub use rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource, WorldgenRandom,
    XoroshiroRandomSource, is_slime_chunk, seed_slime_chunk,
};
