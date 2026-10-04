//! The biome facts terrain finishing reads: a biome's climate, and the zoomed
//! per-block lookup over a quart-resolution biome source.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde_json::Value;

use super::noise::Simplex;
use crate::rng::LegacyRandomSource;

/// An index into a [`BiomeTable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BiomeId(pub u32);

/// The climate fields of one biome.
#[derive(Clone, Debug)]
pub struct BiomeInfo {
    pub name: String,
    pub temperature: f32,
    pub frozen_modifier: bool,
}

/// Biomes by resource name, with their climate.
#[derive(Clone, Debug, Default)]
pub struct BiomeTable {
    biomes: Vec<BiomeInfo>,
    index: HashMap<String, BiomeId>,
}

impl BiomeTable {
    /// Builds a table from `(name, json)` pairs, in the given order.
    ///
    /// # Panics
    /// If a bundled document is malformed, which is a build-data defect.
    pub fn from_tables(tables: &[(&str, &str)]) -> Self {
        let mut t = Self::default();
        for (name, json) in tables {
            let doc: Value = serde_json::from_str(json).expect("bundled biome JSON parses");
            let temperature = doc.get("temperature").and_then(Value::as_f64).expect("biome temperature") as f32;
            let frozen = doc.get("temperature_modifier").and_then(Value::as_str) == Some("frozen");
            let key = format!("minecraft:{name}");
            t.index.insert(key.clone(), BiomeId(t.biomes.len() as u32));
            t.biomes.push(BiomeInfo { name: key, temperature, frozen_modifier: frozen });
        }
        t
    }

    pub fn id(&self, name: &str) -> Option<BiomeId> {
        let key = if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") };
        self.index.get(&key).copied()
    }

    pub fn info(&self, id: BiomeId) -> &BiomeInfo {
        &self.biomes[id.0 as usize]
    }

    pub fn len(&self) -> usize {
        self.biomes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.biomes.is_empty()
    }
}

/// Process-wide climate noises: three fixed-seed simplex fields.
struct ClimateNoise {
    temperature: Simplex,
    /// Layers of (noise, frequency, amplitude).
    frozen: [(Simplex, f64, f32); 3],
    info: Simplex,
}

fn climate_noise() -> &'static ClimateNoise {
    static NOISE: OnceLock<ClimateNoise> = OnceLock::new();
    NOISE.get_or_init(|| {
        let temperature = Simplex::new_discarding_offset(&mut LegacyRandomSource::new(1234));
        let mut r = LegacyRandomSource::new(3456);
        let a = Simplex::new_discarding_offset(&mut r);
        let b = Simplex::new_discarding_offset(&mut r);
        let c = Simplex::new_discarding_offset(&mut r);
        let info = Simplex::new_discarding_offset(&mut LegacyRandomSource::new(2345));
        ClimateNoise { temperature, frozen: [(a, 1.0, 0.142_857_15), (b, 0.5, 0.285_714_3), (c, 0.25, 0.571_428_6)], info }
    })
}

fn frozen_stack(x: f64, y: f64) -> f32 {
    let n = climate_noise();
    let mut value = 0.0f32;
    for (noise, frequency, amplitude) in &n.frozen {
        value += amplitude * noise.get(x * frequency, y * frequency);
    }
    value
}

fn modified_temperature(biome: &BiomeInfo, x: i32, z: i32) -> f32 {
    if !biome.frozen_modifier {
        return biome.temperature;
    }
    let n = climate_noise();
    let large = f64::from(frozen_stack(f64::from(x) * 0.05, f64::from(z) * 0.05) * 7.0f32);
    let edge = f64::from(n.info.get(f64::from(x) * 0.2, f64::from(z) * 0.2));
    if large + edge < 0.3 {
        let small = f64::from(n.info.get(f64::from(x) * 0.09, f64::from(z) * 0.09));
        if small < 0.8 {
            return 0.2;
        }
    }
    biome.temperature
}

/// The biome's temperature at a block, lowered above the snow line.
pub fn temperature_at(biome: &BiomeInfo, x: i32, y: i32, z: i32, sea_level: i32) -> f32 {
    let adjusted = modified_temperature(biome, x, z);
    let snow_level = sea_level + 17;
    if y > snow_level {
        let v = climate_noise().temperature.get(f64::from(x as f32 / 8.0f32), f64::from(z as f32 / 8.0f32)) * 8.0f32;
        adjusted - (v + y as f32 - snow_level as f32) * 0.05f32 / 40.0f32
    } else {
        adjusted
    }
}

pub fn cold_enough_to_snow(biome: &BiomeInfo, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
    !(temperature_at(biome, x, y, z, sea_level) >= 0.15f32)
}

pub fn melts_frozen_ocean_iceberg_slightly(biome: &BiomeInfo, x: i32, y: i32, z: i32, sea_level: i32) -> bool {
    temperature_at(biome, x, y, z, sea_level) > 0.1f32
}

fn next(rval: i64, c: i64) -> i64 {
    rval.wrapping_mul(rval.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407)).wrapping_add(c)
}

fn fiddle(rval: i64) -> f64 {
    let uniform = f64::from(((rval >> 24).rem_euclid(1024)) as i32) / 1024.0;
    (uniform - 0.5) * 0.9
}

fn fiddled_distance(seed: i64, x: i32, y: i32, z: i32, dx: f64, dy: f64, dz: f64) -> f64 {
    let (xr, yr, zr) = (i64::from(x), i64::from(y), i64::from(z));
    let mut r = seed;
    r = next(r, xr);
    r = next(r, yr);
    r = next(r, zr);
    r = next(r, xr);
    r = next(r, yr);
    r = next(r, zr);
    let fx = fiddle(r);
    r = next(r, seed);
    let fy = fiddle(r);
    r = next(r, seed);
    let fz = fiddle(r);
    let sq = |v: f64| v * v;
    sq(dz + fz) + sq(dy + fy) + sq(dx + fx)
}

/// The block-resolution biome at `(x, y, z)`: picks one of the eight quart cells
/// around the block by fiddled distance, then asks `source` (quart coordinates).
/// `zoom_seed` is the obfuscated world seed.
pub fn zoomed_biome<S: FnMut(i32, i32, i32) -> BiomeId + ?Sized>(zoom_seed: i64, x: i32, y: i32, z: i32, source: &mut S) -> BiomeId {
    let (ax, ay, az) = (x - 2, y - 2, z - 2);
    let (px, py, pz) = (ax >> 2, ay >> 2, az >> 2);
    let (fx, fy, fz) = (f64::from(ax & 3) / 4.0, f64::from(ay & 3) / 4.0, f64::from(az & 3) / 4.0);
    let mut min_i = 0;
    let mut min_d = f64::INFINITY;
    for i in 0..8 {
        let x_even = (i & 4) == 0;
        let y_even = (i & 2) == 0;
        let z_even = (i & 1) == 0;
        let (cx, cy, cz) = (if x_even { px } else { px + 1 }, if y_even { py } else { py + 1 }, if z_even { pz } else { pz + 1 });
        let (dx, dy, dz) = (if x_even { fx } else { fx - 1.0 }, if y_even { fy } else { fy - 1.0 }, if z_even { fz } else { fz - 1.0 });
        let d = fiddled_distance(zoom_seed, cx, cy, cz, dx, dy, dz);
        if min_d > d {
            min_i = i;
            min_d = d;
        }
    }
    source(
        if (min_i & 4) == 0 { px } else { px + 1 },
        if (min_i & 2) == 0 { py } else { py + 1 },
        if (min_i & 1) == 0 { pz } else { pz + 1 },
    )
}
