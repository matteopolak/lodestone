//! The climate spawn search: where a new world's spawn search starts.
//!
//! A noise-settings document may list `spawn_target` points: per-parameter
//! intervals over the climate fields (continentalness, erosion, ridges,
//! temperature, vegetation). A block's fitness is the smallest, over the
//! points, of the summed squared distance from each sampled (quantised) value
//! to its interval. The search walks outward rings from the origin, then
//! refines around the best, keeping the position with the lowest fitness
//! weighted by its distance from the origin.

use serde_json::Value;

use super::sampler::{Ctx, SId};
use super::settings::{Router, SettingsError, TerrainGenerator};

/// One climate parameter's quantised closed interval.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpawnInterval {
    root: SId,
    min: i64,
    max: i64,
}

/// One target point: every listed parameter must be near its interval.
pub(crate) type SpawnTarget = Vec<SpawnInterval>;

/// Distance weight: the squared fitness scale of one ring-search radius bound.
const DISTANCE_SCALE: i64 = 2048 * 2048;

/// Parses a settings document's `spawn_target` list into router roots.
pub(crate) fn parse_spawn_targets(doc: &Value, router: &Router) -> Result<Vec<SpawnTarget>, SettingsError> {
    let Some(list) = doc.get("spawn_target") else {
        return Ok(Vec::new());
    };
    let list = list.as_array().ok_or(SettingsError::Malformed("spawn_target"))?;
    let root_of = |name: &str| -> Result<SId, SettingsError> {
        match name.strip_prefix("minecraft:").unwrap_or(name) {
            "overworld/temperature" => Ok(router.temperature),
            "overworld/vegetation" => Ok(router.vegetation),
            "overworld/continents" => Ok(router.continents),
            "overworld/erosion" => Ok(router.erosion),
            "overworld/depth" => Ok(router.depth),
            "overworld/ridges" => Ok(router.ridges),
            _ => Err(SettingsError::Malformed("spawn_target parameter")),
        }
    };
    let quantise = |v: &Value| -> Result<i64, SettingsError> {
        let f = v.as_f64().ok_or(SettingsError::Malformed("spawn_target bound"))? as f32;
        Ok(super::climate::quantize(f))
    };
    list.iter()
        .map(|point| {
            let map = point.as_object().ok_or(SettingsError::Malformed("spawn_target point"))?;
            map.iter()
                .map(|(name, bounds)| {
                    let pair = bounds.as_array().filter(|a| a.len() == 2).ok_or(SettingsError::Malformed("spawn_target interval"))?;
                    Ok(SpawnInterval { root: root_of(name)?, min: quantise(&pair[0])?, max: quantise(&pair[1])? })
                })
                .collect()
        })
        .collect()
}

/// Distance from `value` to the closed interval `[min, max]`; zero inside.
fn interval_distance(value: i64, min: i64, max: i64) -> i64 {
    let above = value - max;
    let below = min - value;
    if above > 0 { above } else { below.max(0) }
}

/// A scored block position.
#[derive(Clone, Copy)]
struct Scored {
    x: i32,
    z: i32,
    fitness: i64,
}

impl TerrainGenerator {
    /// The block column a fresh world's spawn search starts from, or `None`
    /// when the settings carry no spawn targets.
    ///
    /// Positions are scored at their quart-aligned column, height zero.
    pub fn spawn_origin(&self) -> Option<(i32, i32)> {
        if self.spawn_targets.is_empty() {
            return None;
        }
        let mut ctx = Ctx::new(&self.program);
        let mut best = self.score_spawn_position(&mut ctx, 0, 0);
        best = self.radial_spawn_search(&mut ctx, best, 2048.0, 512.0);
        best = self.radial_spawn_search(&mut ctx, best, 512.0, 32.0);
        Some((best.x, best.z))
    }

    fn score_spawn_position(&self, ctx: &mut Ctx, x: i32, z: i32) -> Scored {
        let (qx, qz) = (x >> 2, z >> 2);
        let (bx, bz) = (qx << 2, qz << 2);
        let mut min_fitness = i64::MAX;
        for point in &self.spawn_targets {
            let mut fitness = 0i64;
            for interval in point {
                let value = super::climate::quantize(self.program.value(ctx, interval.root, bx, 0, bz));
                let d = interval_distance(value, interval.min, interval.max);
                fitness += d * d;
            }
            min_fitness = min_fitness.min(fitness);
        }
        let bias = i64::from(x) * i64::from(x) + i64::from(z) * i64::from(z);
        Scored { x, z, fitness: min_fitness * DISTANCE_SCALE + bias }
    }

    /// Rings of single-precision sample points around the incoming best,
    /// spaced by arc length `increment`; replaces the best on a strictly lower
    /// score. The angle and radius are `f32` because the sample positions
    /// depend on that rounding.
    fn radial_spawn_search(&self, ctx: &mut Ctx, start: Scored, max_radius: f32, increment: f32) -> Scored {
        let mut best = start;
        let mut angle = 0.0_f32;
        let mut radius = increment;
        while radius <= max_radius {
            let x = start.x + (f64::from(angle).sin() * f64::from(radius)) as i32;
            let z = start.z + (f64::from(angle).cos() * f64::from(radius)) as i32;
            let candidate = self.score_spawn_position(ctx, x, z);
            if candidate.fitness < best.fitness {
                best = candidate;
            }
            angle += increment / radius;
            if f64::from(angle) > std::f64::consts::PI * 2.0 {
                angle = 0.0;
                radius += increment;
            }
        }
        best
    }
}
