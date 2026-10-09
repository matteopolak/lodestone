//! Perlin/normal noise synthesis in `f64`, bit-exact against the reference's
//! own improved, Perlin and normal noise.
//!
//! The 26.3 terrain engine samples its own `f32` noise
//! (`engine::release26_3::noise`); this module serves the noise-driven block
//! state providers of the features structure pools place.

pub mod improved;
pub mod normal;
pub mod perlin;

pub use improved::ImprovedNoise;
pub use normal::NormalNoise;
pub use perlin::{PerlinNoise, wrap};
