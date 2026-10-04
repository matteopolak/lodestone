//! Cave and canyon carving: each chunk is carved by tunnels and rooms whose seeds
//! come from the 17x17 chunks around it. Carvers write into a mask first; the
//! mask is then applied through the fill's aquifer, so carved cells can flood or
//! fill with lava the way the surrounding terrain does.
//!
//! The arithmetic mixes `f32` and `f64` exactly as the reference does (angles and
//! thicknesses are floats, positions and radii are doubles), and the random draw
//! order is part of the result.

use std::collections::HashMap;

use serde_json::Value;

use super::aquifer::Fluid;
use super::biome::BiomeId;
use super::climate::{BiomeSource, ClimateCursor};
use super::material::{Anchor, GenContext};
use super::sampler::Ctx;
use super::settings::{ChunkFill, TerrainGenerator};
use super::surface::SurfaceChunk;
use crate::math;
use crate::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};

/// How many chunks around a chunk can carve into it.
const RANGE: i32 = 4;
/// Blocks at the top of the world that carving leaves alone.
const PROTECTED_TOP: i32 = 7;

#[derive(Clone, Debug)]
enum FloatProvider {
    Constant(f32),
    Uniform { min: f32, max: f32 },
    Trapezoid { min: f32, max: f32, plateau: f32 },
}

impl FloatProvider {
    fn parse(v: &Value) -> Result<Self, String> {
        if let Some(n) = v.as_f64() {
            return Ok(Self::Constant(n as f32));
        }
        let o = v.as_object().ok_or("float provider must be a number or object")?;
        let f = |k: &str| o.get(k).and_then(Value::as_f64).map(|n| n as f32).ok_or_else(|| format!("float provider missing {k}"));
        match o.get("type").and_then(Value::as_str).map(|t| t.trim_start_matches("minecraft:")) {
            Some("constant") => Ok(Self::Constant(f("value")?)),
            Some("uniform") => Ok(Self::Uniform { min: f("min_inclusive")?, max: f("max_exclusive")? }),
            Some("trapezoid") => Ok(Self::Trapezoid { min: f("min")?, max: f("max")?, plateau: f("plateau")? }),
            other => Err(format!("unsupported float provider {other:?}")),
        }
    }

    fn sample<R: RandomSource>(&self, r: &mut R) -> f32 {
        match *self {
            Self::Constant(v) => v,
            Self::Uniform { min, max } => r.next_float() * (max - min) + min,
            Self::Trapezoid { min, max, plateau } => {
                let range = max - min;
                let plateau_start = (range - plateau) / 2.0;
                let plateau_end = range - plateau_start;
                let a = r.next_float();
                let first = min + a * plateau_end;
                first + r.next_float() * plateau_start
            }
        }
    }
}

#[derive(Clone, Debug)]
enum IntProvider {
    Constant(i32),
    VeryBiasedToBottom { min: i32, max: i32 },
}

impl IntProvider {
    fn parse(v: &Value) -> Result<Self, String> {
        if let Some(n) = v.as_i64() {
            return Ok(Self::Constant(n as i32));
        }
        let o = v.as_object().ok_or("int provider must be a number or object")?;
        let i = |k: &str| o.get(k).and_then(Value::as_i64).map(|n| n as i32).ok_or_else(|| format!("int provider missing {k}"));
        match o.get("type").and_then(Value::as_str).map(|t| t.trim_start_matches("minecraft:")) {
            Some("constant") => Ok(Self::Constant(i("value")?)),
            Some("very_biased_to_bottom") => Ok(Self::VeryBiasedToBottom { min: i("min_inclusive")?, max: i("max_inclusive")? }),
            other => Err(format!("unsupported int provider {other:?}")),
        }
    }

    fn sample<R: RandomSource>(&self, r: &mut R) -> i32 {
        match *self {
            Self::Constant(v) => v,
            Self::VeryBiasedToBottom { min, max } => {
                let a = r.next_int_bounded(max - min + 1) + 1;
                let b = r.next_int_bounded(a) + 1;
                min + r.next_int_bounded(b)
            }
        }
    }
}

#[derive(Clone, Debug)]
struct HeightProvider {
    min: Anchor,
    max: Anchor,
}

impl HeightProvider {
    fn parse(v: &Value) -> Result<Self, String> {
        let o = v.as_object().ok_or("height provider must be an object")?;
        match o.get("type").and_then(Value::as_str).map(|t| t.trim_start_matches("minecraft:")) {
            Some("uniform") => {
                let a = |k: &str| Anchor::parse_value(o.get(k).ok_or_else(|| format!("height provider missing {k}"))?);
                Ok(Self { min: a("min_inclusive")?, max: a("max_inclusive")? })
            }
            other => Err(format!("unsupported height provider {other:?}")),
        }
    }

    fn sample<R: RandomSource>(&self, r: &mut R, g: GenContext) -> i32 {
        let (min, max) = (self.min.resolve(g), self.max.resolve(g));
        if min > max { min } else { r.next_int_bounded(max - min + 1) + min }
    }
}

#[derive(Clone, Debug)]
struct CaveCarver {
    probability: f32,
    y: HeightProvider,
    count: IntProvider,
    thickness: FloatProvider,
    weird_thickness_bias: bool,
    room_vertical_radius_multiplier: FloatProvider,
    horizontal_radius_multiplier: FloatProvider,
    vertical_radius_multiplier: FloatProvider,
    start_vertical_radius_multiplier: FloatProvider,
    floor_level: FloatProvider,
}

#[derive(Clone, Debug)]
struct CanyonShape {
    distance_factor: FloatProvider,
    thickness: FloatProvider,
    width_smoothness: i32,
    horizontal_radius_factor: FloatProvider,
    vertical_radius_default_factor: f32,
    vertical_radius_center_factor: f32,
    y_scale: FloatProvider,
}

#[derive(Clone, Debug)]
struct CanyonCarver {
    probability: f32,
    y: HeightProvider,
    vertical_rotation: FloatProvider,
    shape: CanyonShape,
}

#[derive(Clone, Debug)]
enum Carver {
    Cave(CaveCarver),
    Canyon(CanyonCarver),
}

/// Parsed carver documents by resource name.
#[derive(Clone, Debug, Default)]
pub struct CarverTable {
    carvers: HashMap<String, Carver>,
}

impl CarverTable {
    /// Parses `(name, json)` pairs.
    ///
    /// # Errors
    /// On a malformed document or a provider type the engine does not implement.
    pub fn from_tables(tables: &[(&str, &str)]) -> Result<Self, String> {
        let mut carvers = HashMap::new();
        for (name, json) in tables {
            let doc: Value = serde_json::from_str(json).map_err(|e| format!("{name}: {e}"))?;
            carvers.insert(format!("minecraft:{name}"), Carver::parse(&doc).map_err(|e| format!("{name}: {e}"))?);
        }
        Ok(Self { carvers })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.carvers.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.carvers.is_empty()
    }
}

impl Carver {
    fn parse(doc: &Value) -> Result<Self, String> {
        let get = |k: &str| doc.get(k).ok_or_else(|| format!("missing {k}"));
        let prob = get("probability")?.as_f64().ok_or("probability")? as f32;
        let fp = |k: &str| FloatProvider::parse(get(k)?);
        match doc.get("type").and_then(Value::as_str).map(|t| t.trim_start_matches("minecraft:")) {
            Some("cave") => Ok(Self::Cave(CaveCarver {
                probability: prob,
                y: HeightProvider::parse(get("y")?)?,
                count: IntProvider::parse(get("count")?)?,
                thickness: fp("thickness")?,
                weird_thickness_bias: doc.get("weird_thickness_bias").and_then(Value::as_bool).unwrap_or(false),
                room_vertical_radius_multiplier: fp("room_vertical_radius_multiplier")?,
                horizontal_radius_multiplier: fp("horizontal_radius_multiplier")?,
                vertical_radius_multiplier: fp("vertical_radius_multiplier")?,
                start_vertical_radius_multiplier: match doc.get("start_vertical_radius_multiplier") {
                    Some(v) => FloatProvider::parse(v)?,
                    None => FloatProvider::Constant(1.0),
                },
                floor_level: fp("floor_level")?,
            })),
            Some("canyon") => {
                let shape = get("shape")?;
                let sg = |k: &str| shape.get(k).ok_or_else(|| format!("shape missing {k}"));
                let sf = |k: &str| sg(k)?.as_f64().map(|n| n as f32).ok_or_else(|| format!("shape {k}"));
                Ok(Self::Canyon(CanyonCarver {
                    probability: prob,
                    y: HeightProvider::parse(get("y")?)?,
                    vertical_rotation: fp("vertical_rotation")?,
                    shape: CanyonShape {
                        distance_factor: FloatProvider::parse(sg("distance_factor")?)?,
                        thickness: FloatProvider::parse(sg("thickness")?)?,
                        width_smoothness: sg("width_smoothness")?.as_i64().ok_or("width_smoothness")? as i32,
                        horizontal_radius_factor: FloatProvider::parse(sg("horizontal_radius_factor")?)?,
                        vertical_radius_default_factor: sf("vertical_radius_default_factor")?,
                        vertical_radius_center_factor: sf("vertical_radius_center_factor")?,
                        y_scale: FloatProvider::parse(sg("y_scale")?)?,
                    },
                }))
            }
            other => Err(format!("unsupported carver type {other:?}")),
        }
    }

    fn is_start_chunk<R: RandomSource>(&self, r: &mut R) -> bool {
        let p = match self {
            Self::Cave(c) => c.probability,
            Self::Canyon(c) => c.probability,
        };
        r.next_float() <= p
    }
}

/// The cells a chunk's carvers removed, bottom exclusive of the dimension's lowest block.
#[derive(Debug)]
pub struct CarvingMask {
    min_y: i32,
    max_y: i32,
    height: i32,
    bits: Vec<bool>,
}

impl CarvingMask {
    fn new(min_y: i32, max_y: i32) -> Self {
        let height = max_y - min_y + 1;
        Self { min_y, max_y, height, bits: vec![false; (256 * height).max(0) as usize] }
    }

    fn index(&self, x: i32, y: i32, z: i32) -> usize {
        (y - self.min_y + (z + (x << 4)) * self.height) as usize
    }

    fn carve(&mut self, x: i32, y: i32, z: i32) {
        let i = self.index(x, y, z);
        self.bits[i] = true;
    }

    fn get(&self, x: i32, y: i32, z: i32) -> bool {
        self.bits[self.index(x, y, z)]
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.bits.iter().any(|&b| b)
    }
}

struct Site {
    chunk_x: i32,
    chunk_z: i32,
}

fn carve_ellipsoid(
    site: &Site,
    x: f64,
    y: f64,
    z: f64,
    horizontal_radius: f64,
    vertical_radius: f64,
    mask: &mut CarvingMask,
    skip: &dyn Fn(f64, f64, f64, i32) -> bool,
) {
    let center_x = f64::from(site.chunk_x * 16 + 8);
    let center_z = f64::from(site.chunk_z * 16 + 8);
    let max_delta = 16.0 + horizontal_radius * 2.0;
    if (x - center_x).abs() > max_delta || (z - center_z).abs() > max_delta {
        return;
    }
    let (chunk_min_x, chunk_min_z) = (site.chunk_x * 16, site.chunk_z * 16);
    let min_x = (math::floor(x - horizontal_radius) - chunk_min_x - 1).max(0);
    let max_x = (math::floor(x + horizontal_radius) - chunk_min_x).min(15);
    let min_y = (math::floor(y - vertical_radius) - 1).max(mask.min_y);
    let max_y = (math::floor(y + vertical_radius) + 1).min(mask.max_y);
    let min_z = (math::floor(z - horizontal_radius) - chunk_min_z - 1).max(0);
    let max_z = (math::floor(z + horizontal_radius) - chunk_min_z).min(15);
    for xi in min_x..=max_x {
        let xd = (f64::from(chunk_min_x + xi) + 0.5 - x) / horizontal_radius;
        for zi in min_z..=max_z {
            let zd = (f64::from(chunk_min_z + zi) + 0.5 - z) / horizontal_radius;
            if xd * xd + zd * zd >= 1.0 {
                continue;
            }
            let mut wy = max_y;
            while wy > min_y {
                let yd = (f64::from(wy) - 0.5 - y) / vertical_radius;
                if !skip(xd, yd, zd, wy) {
                    mask.carve(xi, wy, zi);
                }
                wy -= 1;
            }
        }
    }
}

fn can_reach(site: &Site, x: f64, z: f64, step: i32, total: i32, thickness: f32) -> bool {
    let xd = x - f64::from(site.chunk_x * 16 + 8);
    let zd = z - f64::from(site.chunk_z * 16 + 8);
    let remaining = f64::from(total - step);
    let rr = f64::from(thickness + 2.0_f32 + 16.0_f32);
    xd * xd + zd * zd - remaining * remaining <= rr * rr
}

const TAU_F: f32 = (std::f64::consts::PI * 2.0) as f32;
const HALF_PI_F: f32 = (std::f64::consts::PI / 2.0) as f32;
const PI_F: f32 = std::f64::consts::PI as f32;

impl CaveCarver {
    #[allow(clippy::too_many_arguments)]
    fn carve<R: RandomSource>(&self, g: GenContext, r: &mut R, site: &Site, source: [i32; 2], mask: &mut CarvingMask) {
        let max_distance = (RANGE * 2 - 1) * 16;
        let count = self.count.sample(r);
        for _ in 0..count {
            let x = f64::from(source[0] * 16 + r.next_int_bounded(16));
            let y = f64::from(self.y.sample(r, g));
            let z = f64::from(source[1] * 16 + r.next_int_bounded(16));
            let horizontal = f64::from(self.horizontal_radius_multiplier.sample(r));
            let vertical = f64::from(self.vertical_radius_multiplier.sample(r));
            let start_vertical = f64::from(self.start_vertical_radius_multiplier.sample(r));
            let floor_level = f64::from(self.floor_level.sample(r));
            let skip = move |xd: f64, yd: f64, zd: f64, _y: i32| if yd <= floor_level { true } else { xd * xd + yd * yd + zd * zd >= 1.0 };
            let mut tunnels = 1;
            if r.next_int_bounded(4) == 0 {
                let y_scale = f64::from(self.room_vertical_radius_multiplier.sample(r));
                let thickness = 1.0_f32 + r.next_float() * 6.0_f32;
                let horizontal_radius = 1.5 + f64::from(math::sin(f64::from(HALF_PI_F)) * thickness);
                let vertical_radius = horizontal_radius * y_scale;
                carve_ellipsoid(site, x + 1.0, y, z, horizontal_radius, vertical_radius, mask, &skip);
                tunnels += r.next_int_bounded(4);
            }
            for _ in 0..tunnels {
                let horizontal_rotation = r.next_float() * TAU_F;
                let vertical_rotation = (r.next_float() - 0.5_f32) / 4.0_f32;
                let thickness = self.thickness_sample(r);
                let distance = max_distance - r.next_int_bounded(max_distance / 4);
                let seed = r.next_long();
                self.create_tunnel(
                    site, seed, [x, y, z], [horizontal, vertical], thickness, [horizontal_rotation, vertical_rotation], 0, distance,
                    start_vertical, mask, &skip,
                );
            }
        }
    }

    fn thickness_sample<R: RandomSource>(&self, r: &mut R) -> f32 {
        let mut thickness = self.thickness.sample(r);
        if self.weird_thickness_bias && r.next_int_bounded(10) == 0 {
            let a = r.next_float();
            let b = r.next_float();
            thickness *= a * b * 3.0_f32 + 1.0_f32;
        }
        thickness
    }

    #[allow(clippy::too_many_arguments)]
    fn create_tunnel(
        &self,
        site: &Site,
        tunnel_seed: i64,
        pos: [f64; 3],
        multipliers: [f64; 2],
        thickness: f32,
        rotation: [f32; 2],
        step: i32,
        dist: i32,
        y_scale: f64,
        mask: &mut CarvingMask,
        skip: &dyn Fn(f64, f64, f64, i32) -> bool,
    ) {
        let [mut x, mut y, mut z] = pos;
        let [mut horizontal_rotation, mut vertical_rotation] = rotation;
        let mut r = LegacyRandomSource::new(tunnel_seed);
        let split_point = r.next_int_bounded(dist / 2) + dist / 4;
        let steep = r.next_int_bounded(6) == 0;
        let mut y_rota = 0.0_f32;
        let mut x_rota = 0.0_f32;
        let mut current = step;
        while current < dist {
            let horizontal_radius = 1.5 + f64::from(math::sin(f64::from(PI_F * current as f32 / dist as f32)) * thickness);
            let vertical_radius = horizontal_radius * y_scale;
            let cos_x = math::cos(f64::from(vertical_rotation));
            x += f64::from(math::cos(f64::from(horizontal_rotation)) * cos_x);
            y += f64::from(math::sin(f64::from(vertical_rotation)));
            z += f64::from(math::sin(f64::from(horizontal_rotation)) * cos_x);
            vertical_rotation *= if steep { 0.92_f32 } else { 0.7_f32 };
            vertical_rotation += x_rota * 0.1_f32;
            horizontal_rotation += y_rota * 0.1_f32;
            x_rota *= 0.9_f32;
            y_rota *= 0.75_f32;
            let (a, b, c) = (r.next_float(), r.next_float(), r.next_float());
            x_rota += (a - b) * c * 2.0_f32;
            let (a, b, c) = (r.next_float(), r.next_float(), r.next_float());
            y_rota += (a - b) * c * 4.0_f32;
            if current == split_point && thickness > 1.0_f32 {
                let seed_a = r.next_long();
                let thick_a = r.next_float() * 0.5_f32 + 0.5_f32;
                self.create_tunnel(
                    site, seed_a, [x, y, z], multipliers, thick_a,
                    [horizontal_rotation - HALF_PI_F, vertical_rotation / 3.0_f32], current, dist, 1.0, mask, skip,
                );
                let seed_b = r.next_long();
                let thick_b = r.next_float() * 0.5_f32 + 0.5_f32;
                self.create_tunnel(
                    site, seed_b, [x, y, z], multipliers, thick_b,
                    [horizontal_rotation + HALF_PI_F, vertical_rotation / 3.0_f32], current, dist, 1.0, mask, skip,
                );
                return;
            }
            if r.next_int_bounded(4) != 0 {
                if !can_reach(site, x, z, current, dist, thickness) {
                    return;
                }
                carve_ellipsoid(site, x, y, z, horizontal_radius * multipliers[0], vertical_radius * multipliers[1], mask, skip);
            }
            current += 1;
        }
    }
}

impl CanyonCarver {
    fn carve<R: RandomSource>(&self, g: GenContext, r: &mut R, site: &Site, source: [i32; 2], mask: &mut CarvingMask) {
        let max_distance = (RANGE * 2 - 1) * 16;
        let x = f64::from(source[0] * 16 + r.next_int_bounded(16));
        let y = self.y.sample(r, g);
        let z = f64::from(source[1] * 16 + r.next_int_bounded(16));
        let horizontal_rotation = r.next_float() * TAU_F;
        let vertical_rotation = self.vertical_rotation.sample(r);
        let y_scale = f64::from(self.shape.y_scale.sample(r));
        let thickness = self.shape.thickness.sample(r);
        let distance = (max_distance as f32 * self.shape.distance_factor.sample(r)) as i32;
        let seed = r.next_long();
        self.do_carve(g, site, seed, [x, f64::from(y), z], thickness, [horizontal_rotation, vertical_rotation], distance, y_scale, mask);
    }

    #[allow(clippy::too_many_arguments)]
    fn do_carve(
        &self,
        g: GenContext,
        site: &Site,
        tunnel_seed: i64,
        pos: [f64; 3],
        thickness: f32,
        rotation: [f32; 2],
        distance: i32,
        y_scale: f64,
        mask: &mut CarvingMask,
    ) {
        let [mut x, mut y, mut z] = pos;
        let [mut horizontal_rotation, mut vertical_rotation] = rotation;
        let mut r = LegacyRandomSource::new(tunnel_seed);
        let widths = self.width_factors(g, &mut r);
        let mut y_rota = 0.0_f32;
        let mut x_rota = 0.0_f32;
        for current in 0..distance {
            let mut horizontal_radius = 1.5 + f64::from(math::sin(f64::from(current as f32 * PI_F / distance as f32)) * thickness);
            let mut vertical_radius = horizontal_radius * y_scale;
            horizontal_radius *= f64::from(self.shape.horizontal_radius_factor.sample(&mut r));
            vertical_radius = self.update_vertical_radius(&mut r, vertical_radius, distance as f32, current as f32);
            let xc = math::cos(f64::from(vertical_rotation));
            let xs = math::sin(f64::from(vertical_rotation));
            x += f64::from(math::cos(f64::from(horizontal_rotation)) * xc);
            y += f64::from(xs);
            z += f64::from(math::sin(f64::from(horizontal_rotation)) * xc);
            vertical_rotation *= 0.7_f32;
            vertical_rotation += x_rota * 0.05_f32;
            horizontal_rotation += y_rota * 0.05_f32;
            x_rota *= 0.8_f32;
            y_rota *= 0.5_f32;
            let (a, b, c) = (r.next_float(), r.next_float(), r.next_float());
            x_rota += (a - b) * c * 2.0_f32;
            let (a, b, c) = (r.next_float(), r.next_float(), r.next_float());
            y_rota += (a - b) * c * 4.0_f32;
            if r.next_int_bounded(4) != 0 {
                if !can_reach(site, x, z, current, distance, thickness) {
                    return;
                }
                let min_gen_y = g.min_y;
                let skip = |xd: f64, yd: f64, zd: f64, wy: i32| {
                    let index = (wy - min_gen_y) as usize;
                    (xd * xd + zd * zd) * f64::from(widths[index - 1]) + yd * yd / 6.0 >= 1.0
                };
                carve_ellipsoid(site, x, y, z, horizontal_radius, vertical_radius, mask, &skip);
            }
        }
    }

    fn width_factors<R: RandomSource>(&self, g: GenContext, r: &mut R) -> Vec<f32> {
        let mut out = vec![0.0_f32; g.height as usize];
        let mut width = 1.0_f32;
        for (i, slot) in out.iter_mut().enumerate() {
            if i == 0 || r.next_int_bounded(self.shape.width_smoothness) == 0 {
                let a = r.next_float();
                width = 1.0_f32 + a * r.next_float();
            }
            *slot = width * width;
        }
        out
    }

    fn update_vertical_radius<R: RandomSource>(&self, r: &mut R, vertical_radius: f64, distance: f32, step: f32) -> f64 {
        let multiplier = 1.0_f32 - (0.5_f32 - step / distance).abs() * 2.0_f32;
        let factor = self.shape.vertical_radius_default_factor + self.shape.vertical_radius_center_factor * multiplier;
        let between = r.next_float() * (1.0_f32 - 0.75_f32) + 0.75_f32;
        f64::from(factor) * vertical_radius * f64::from(between)
    }
}

impl TerrainGenerator {
    /// Carves a chunk that has been filled and surfaced.
    ///
    /// `source` is the biome source the carver lists are read from (sampled at
    /// each neighbouring chunk's corner, at Y 0, without caches); `zoomed` biome
    /// lookups for re-dressing grass use the same source. `ctx` must be the
    /// context the fill and surface used, and `fill` the fill's own result, since
    /// carving continues from its aquifer.
    #[allow(clippy::too_many_arguments)]
    pub fn carve_chunk(
        &self,
        carvers: &CarverTable,
        source: &BiomeSource,
        cursor: &mut ClimateCursor,
        chunk_x: i32,
        chunk_z: i32,
        fill: &mut ChunkFill,
        chunk: &mut SurfaceChunk,
        ctx: &mut Ctx,
    ) {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let g = GenContext {
            min_y: chunk.min_y.max(self.min_y),
            height: chunk.height.min(self.height),
            sea_level: sys.sea_level,
        };
        let mut mask = CarvingMask::new(g.min_y + 1, g.min_y + g.height - 1 - PROTECTED_TOP);
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
        let mut climate_ctx = Ctx::uncached();
        let site = Site { chunk_x, chunk_z };
        for dx in -8..=8 {
            for dz in -8..=8 {
                let (sx, sz) = (chunk_x + dx, chunk_z + dz);
                let biome = self.biome_at_quart(source, cursor, sx * 4, 0, sz * 4, &mut climate_ctx);
                for (index, name) in self.biomes.info(biome).carvers.iter().enumerate() {
                    let Some(carver) = carvers.carvers.get(name) else { continue };
                    random.set_large_feature_seed(self.seed.wrapping_add(index as i64), sx, sz);
                    if carver.is_start_chunk(&mut random) {
                        match carver {
                            Carver::Cave(c) => c.carve(g, &mut random, &site, [sx, sz], &mut mask),
                            Carver::Canyon(c) => c.carve(g, &mut random, &site, [sx, sz], &mut mask),
                        }
                    }
                }
            }
        }
        if mask.is_empty() {
            return;
        }
        self.apply_mask(&mask, source, cursor, chunk_x, chunk_z, fill, chunk, ctx, &mut climate_ctx, g);
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_mask(
        &self,
        mask: &CarvingMask,
        source: &BiomeSource,
        cursor: &mut ClimateCursor,
        chunk_x: i32,
        chunk_z: i32,
        fill: &mut ChunkFill,
        chunk: &mut SurfaceChunk,
        ctx: &mut Ctx,
        climate_ctx: &mut Ctx,
        g: GenContext,
    ) {
        let sys = self.material.as_ref().expect("material rules are loaded");
        let picker = super::aquifer::FluidPicker::new(self.sea_level, self.default_fluid);
        let state_of = |f: Fluid| match f {
            Fluid::Air => sys.air,
            Fluid::Water => sys.water,
            Fluid::Lava => sys.lava,
        };
        for x in 0..16 {
            for z in 0..16 {
                let mut y = mask.min_y;
                while y <= mask.max_y {
                    if !mask.get(x, y, z) {
                        y += 1;
                        continue;
                    }
                    let bottom = y;
                    while y + 1 <= mask.max_y && mask.get(x, y + 1, z) {
                        y += 1;
                    }
                    let top = y;
                    y += 1;
                    let (wx, wz) = (chunk_x * 16 + x, chunk_z * 16 + z);
                    let mut has_grass = false;
                    for wy in (bottom..=top).rev() {
                        let Some(here) = chunk.get(x, wy, z) else { continue };
                        if sys.states.is_block(here, "minecraft:bedrock") {
                            continue;
                        }
                        if sys.states.is_block(here, "minecraft:grass_block") || sys.states.is_block(here, "minecraft:mycelium") {
                            has_grass = true;
                        }
                        let (fluid, schedule) = match fill.aquifer.as_mut() {
                            Some(a) => {
                                let f = a.compute_substance(&self.program, ctx, wx, wy, wz, 0.0);
                                (f, a.should_schedule_fluid_update())
                            }
                            None => (Some(picker.compute(wx, wy, wz).at(wy)), false),
                        };
                        let Some(fluid) = fluid else { continue };
                        let state = state_of(fluid);
                        chunk.set_block_unmarked(sys, x, wy, z, state);
                        let state_has_fluid = sys.states.has_fluid(state);
                        if schedule && state_has_fluid {
                            chunk.mark(x, wy, z);
                        }
                        if has_grass {
                            let below = wy - 1;
                            if chunk.get(x, below, z).is_some_and(|b| sys.states.is_block(b, "minecraft:dirt")) {
                                let mut zoomed = |bx: i32, by: i32, bz: i32| -> BiomeId {
                                    super::biome::zoomed_biome(self.zoom_seed, bx, by, bz, &mut |qx, qy, qz| {
                                        self.biome_at_quart(source, cursor, qx, qy, qz, climate_ctx)
                                    })
                                };
                                let top_state = self.top_material(chunk, ctx, &mut zoomed, g, [wx, below, wz], [x, z], state_has_fluid);
                                if let Some(m) = top_state {
                                    chunk.set_block_unmarked(sys, x, below, z, m);
                                    if sys.states.has_fluid(m) {
                                        chunk.mark(x, below, z);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
