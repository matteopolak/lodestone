//! Particle emitters grouped by effect family.
///
/// The public functions remain re-exported from this module for compatibility;
/// implementation details live in focused modules so each emitter family stays
/// reviewable and independently navigable.
use crate::rng::JavaRandom;
use crate::{
    Behaviour, DripKind, DripPhase, Layer, Particle, ParticleEngine, Sheet, SpriteSource,
};

fn rng_next(engine: &mut ParticleEngine) -> f32 {
    engine.rng().next_f32()
}

fn gaussian(engine: &mut ParticleEngine) -> f64 {
    let u1 = engine.rng().next_f64().max(1e-12);
    let u2 = engine.rng().next_f64();
    (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
}

mod blocks;
mod combat;
mod social;
mod ambient;
mod environment;
mod magic;
mod water;
mod foliage;
mod fire;
mod spark;
mod late;
mod special;

pub use blocks::*;
pub use combat::*;
pub use social::*;
pub use ambient::*;
pub use environment::*;
pub use magic::*;
pub use water::*;
pub use foliage::*;
pub use fire::*;
pub use spark::*;
pub use late::*;
pub use special::*;

#[cfg(test)]
mod tests;
