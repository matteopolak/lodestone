//! 32-bit density-function engine for the 26.3 worldgen schema.
//!
//! The reference evaluates every density function in `f32`, through a compiled
//! sampler graph with a scalar and a bulk path that round differently. This module
//! reproduces that structure rather than the arithmetic alone: see `sampler.rs`.

pub mod aquifer;
pub mod compile;
pub mod interval;
pub mod noise;
pub mod sampler;
pub mod settings;
pub mod tree;
pub mod volume;
