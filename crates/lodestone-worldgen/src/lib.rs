//! Minecraft Java Edition world generation for the integrated server.
//!
//! # Layout
//!
//! * [`terrain263`] — the production generator: 26.3 terrain, biomes, carvers
//!   and placed-feature decoration for every dimension, plus structure starts
//!   and placement over it.
//! * [`structure`] — structure starts, piece generators (jigsaw, coded and
//!   template-based), templates and their processors, and the beardifier that
//!   adapts terrain around them.
//! * [`flat`], [`debug`] — the superflat and debug-world generators.
//! * [`generator`] — the plugin-facing [`generator::ChunkGenerator`] seam.
//! * [`spawners`], [`spawn_stage`] — biome mob-spawn tables and the
//!   generation-time spawn pass.
//! * [`dense_grid`], [`generated_storage`] — the dense block field structure
//!   placement writes into, and the compact sectioned storage a served column
//!   adopts.
//! * [`table_resolver`] — lookup over the bundled worldgen JSON and structure
//!   templates.
//! * [`block_entities`] — block entities generation produces (beehives,
//!   dungeon chests and spawners).
//! * Private: `feature` (the feature placers structures invoke) and
//!   `block_tag` (block-tag closure over a resolver).
//!
//! # The numeric core is a separate crate
//!
//! `counters`, `density`, `engine`, `hash`, `math`, `noise` and `rng` live in
//! `lodestone-worldgen-core` and are re-exported below under these paths, so
//! `lodestone_worldgen::density::Resolver` resolves from either crate. Add
//! numeric/kernel code there and pipeline code here.

pub mod block_entities;
mod block_tag;
pub mod debug;
pub mod dense_grid;
mod feature;
pub mod flat;
pub mod generator;
pub mod generated_storage;
pub mod spawn_stage;
pub mod spawners;
pub mod structure;
pub mod table_resolver;
pub mod terrain263;

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
