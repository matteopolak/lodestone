//! Perlin/normal noise synthesis, version-free and bit-exact against vanilla's
//! vanilla's own noise-synthesis package.

pub mod blended;
pub mod end_islands;
pub mod improved;
pub mod normal;
pub mod perlin;
pub mod perlin_simplex;
pub mod simplex;

pub use blended::BlendedNoise;
pub use end_islands::EndIslandNoise;
pub use improved::ImprovedNoise;
pub use normal::NormalNoise;
pub use perlin::{PerlinNoise, wrap};
pub use perlin_simplex::{ClimateNoise, PerlinSimplexNoise};
pub use simplex::{SimplexNoise, biome_info_noise_value};
