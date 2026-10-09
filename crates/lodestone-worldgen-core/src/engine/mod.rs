//! The terrain engine.
//!
//! [`release26_3`] is the only engine: the density functions, aquifers,
//! biomes, carvers and surface rules of the 26.3 worldgen schema, evaluated in
//! `f32` the way the reference does. `lodestone_worldgen::terrain263` drives it
//! for every dimension.

pub mod release26_3;
