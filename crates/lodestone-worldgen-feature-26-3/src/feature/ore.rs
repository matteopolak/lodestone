//! Ore blobs (`ore`) and scattered ore (`scattered_ore`).

use lodestone_worldgen_core::math;
use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{Dir, State};
use crate::env::{Env, Heightmap};
use crate::json::{Res, array, boolean, float_or, get, int};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::{RuleTest, state_of};

#[derive(Clone, Debug)]
pub struct OreConfig {
    pub targets: Vec<(RuleTest, State)>,
    pub size: i32,
    pub discard_chance_on_air_exposure: f32,
}

impl OreConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let mut targets = Vec::new();
        for t in array(v, "targets", ctx)? {
            targets.push((RuleTest::parse(env, get(t, "target", ctx)?, ctx)?, state_of(env, get(t, "state", ctx)?, ctx)?));
        }
        let _ = boolean;
        Ok(Self { targets, size: int(v, "size", ctx)?, discard_chance_on_air_exposure: float_or(v, "discard_chance_on_air_exposure", 0.0, ctx)? })
    }
}

fn is_adjacent_to_air(level: &Level<'_>, x: i32, y: i32, z: i32) -> bool {
    Dir::ALL.iter().any(|d| {
        let (dx, dy, dz) = d.step();
        level.env.blocks.is_air(level.get(x + dx, y + dy, z + dz))
    })
}

fn should_skip_air_check(rng: &mut Rng, discard: f32) -> bool {
    if discard <= 0.0 {
        true
    } else if discard >= 1.0 {
        false
    } else {
        rng.next_float() >= discard
    }
}

/// The first target that accepts `state` at `(x, y, z)`, drawing the air-exposure roll for each
/// target whose test passes (as the reference does).
fn matching_target(cfg: &OreConfig, level: &Level<'_>, rng: &mut Rng, state: State, x: i32, y: i32, z: i32) -> Option<State> {
    for (test, out) in &cfg.targets {
        if !test.test(level.env, state, y) {
            continue;
        }
        if should_skip_air_check(rng, cfg.discard_chance_on_air_exposure) || !is_adjacent_to_air(level, x, y, z) {
            return Some(*out);
        }
    }
    None
}

fn ceil_f32(v: f32) -> i32 {
    let i = v as i32;
    if v > i as f32 { i + 1 } else { i }
}

pub fn place_ore(cfg: &OreConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let pi = std::f32::consts::PI;
    let dir = rng.next_float() * pi;
    let size = cfg.size;
    let spread_xy = size as f32 / 8.0;
    let max_radius = ceil_f32((size as f32 / 16.0 * 2.0 + 1.0) / 2.0);
    let sin = f64::from(dir).sin();
    let cos = f64::from(dir).cos();
    let x0 = f64::from(origin.x) + sin * f64::from(spread_xy);
    let x1 = f64::from(origin.x) - sin * f64::from(spread_xy);
    let z0 = f64::from(origin.z) + cos * f64::from(spread_xy);
    let z1 = f64::from(origin.z) - cos * f64::from(spread_xy);
    let y0 = f64::from(origin.y + rng.next_int_bounded(3) - 2);
    let y1 = f64::from(origin.y + rng.next_int_bounded(3) - 2);
    let x_start = origin.x - ceil_f32(spread_xy) - max_radius;
    let y_start = origin.y - 2 - max_radius;
    let z_start = origin.z - ceil_f32(spread_xy) - max_radius;
    let size_xz = 2 * (ceil_f32(spread_xy) + max_radius);
    let size_y = 2 * (2 + max_radius);
    for xp in x_start..=x_start + size_xz {
        for zp in z_start..=z_start + size_xz {
            if y_start <= level.height(Heightmap::OceanFloorWg, xp, zp) {
                return do_place(cfg, level, rng, [x0, x1, z0, z1, y0, y1], [x_start, y_start, z_start], size_xz, size_y);
            }
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn do_place(cfg: &OreConfig, level: &mut Level<'_>, rng: &mut Rng, c: [f64; 6], start: [i32; 3], size_xz: i32, size_y: i32) -> bool {
    let [x0, x1, z0, z1, y0, y1] = c;
    let [x_start, y_start, z_start] = start;
    let size = cfg.size as usize;
    let mut placed = 0;
    let mut tested = vec![false; (size_xz * size_y * size_xz) as usize];
    let mut data = vec![0.0f64; size * 4];
    let lerp = |t: f64, a: f64, b: f64| a + t * (b - a);
    for i in 0..size {
        let step = i as f32 / cfg.size as f32;
        let t = f64::from(step);
        let (xx, yy, zz) = (lerp(t, x0, x1), lerp(t, y0, y1), lerp(t, z0, z1));
        let ss = rng.next_double() * f64::from(cfg.size) / 16.0;
        let r = (f64::from(math::sin(f64::from(std::f32::consts::PI * step)) + 1.0f32) * ss + 1.0) / 2.0;
        data[i * 4] = xx;
        data[i * 4 + 1] = yy;
        data[i * 4 + 2] = zz;
        data[i * 4 + 3] = r;
    }
    for i1 in 0..size.saturating_sub(1) {
        if data[i1 * 4 + 3] <= 0.0 {
            continue;
        }
        for i2 in i1 + 1..size {
            if data[i2 * 4 + 3] <= 0.0 {
                continue;
            }
            let dx = data[i1 * 4] - data[i2 * 4];
            let dy = data[i1 * 4 + 1] - data[i2 * 4 + 1];
            let dz = data[i1 * 4 + 2] - data[i2 * 4 + 2];
            let dr = data[i1 * 4 + 3] - data[i2 * 4 + 3];
            if dr * dr > dx * dx + dy * dy + dz * dz {
                if dr > 0.0 {
                    data[i2 * 4 + 3] = -1.0;
                } else {
                    data[i1 * 4 + 3] = -1.0;
                }
            }
        }
    }
    for i in 0..size {
        let r = data[i * 4 + 3];
        if r < 0.0 {
            continue;
        }
        let (xx, yy, zz) = (data[i * 4], data[i * 4 + 1], data[i * 4 + 2]);
        let x_min = math::floor(xx - r).max(x_start);
        let y_min = math::floor(yy - r).max(y_start);
        let z_min = math::floor(zz - r).max(z_start);
        let x_max = math::floor(xx + r).max(x_min);
        let y_max = math::floor(yy + r).max(y_min);
        let z_max = math::floor(zz + r).max(z_min);
        for x in x_min..=x_max {
            let xd = (f64::from(x) + 0.5 - xx) / r;
            if xd * xd >= 1.0 {
                continue;
            }
            for y in y_min..=y_max {
                let yd = (f64::from(y) + 0.5 - yy) / r;
                if xd * xd + yd * yd >= 1.0 {
                    continue;
                }
                for z in z_min..=z_max {
                    let zd = (f64::from(z) + 0.5 - zz) / r;
                    if xd * xd + yd * yd + zd * zd < 1.0 && !level.is_outside_build_height(y) {
                        let idx = ((x - x_start) + (y - y_start) * size_xz + (z - z_start) * size_xz * size_y) as usize;
                        if !tested[idx] {
                            tested[idx] = true;
                            if level.can_write(x, z) {
                                let state = level.get(x, y, z);
                                if let Some(out) = matching_target(cfg, level, rng, state, x, y, z) {
                                    level.set_raw(x, y, z, out);
                                    placed += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    placed > 0
}

fn offset_axis(rng: &mut Rng, max_dist: i32) -> i32 {
    let a = rng.next_float();
    let b = rng.next_float();
    // `Math.round(float)`: floor(x + 1/2), exact in double for these magnitudes.
    (f64::from((a - b) * max_dist as f32) + 0.5).floor() as i32
}

pub fn place_scattered(cfg: &OreConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let tries = rng.next_int_bounded(cfg.size + 1);
    for i in 0..tries {
        let max = i.min(7);
        let xd = offset_axis(rng, max);
        let yd = offset_axis(rng, max);
        let zd = offset_axis(rng, max);
        let (x, y, z) = (origin.x + xd, origin.y + yd, origin.z + zd);
        let state = level.get(x, y, z);
        if let Some(out) = matching_target(cfg, level, rng, state, x, y, z) {
            level.set(x, y, z, out);
        }
    }
    true
}
