//! Ice features: `spike` (ice spikes), `iceberg` and `blue_ice`.
//!
//! The iceberg mixes `f32` and `f64` the way the reference does (radius and scale are floats,
//! the signed distances doubles); each draw happens at the same point in the evaluation.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use crate::blocks::{BlockId, Dir, State};
use crate::env::Env;
use crate::json::{Res, get};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::predicate::BlockPred;
use crate::stateprovider::StateProvider;

fn block(env: &Env, name: &str) -> BlockId {
    env.blocks.block_by_name(name).unwrap_or_else(|| panic!("block {name}"))
}

fn state(env: &Env, name: &str) -> State {
    env.blocks.default_state(block(env, name))
}

fn is(env: &Env, s: State, name: &str) -> bool {
    env.blocks.block_of(s) == block(env, name)
}

#[derive(Clone, Debug)]
pub struct SpikeConfig {
    pub state: State,
    pub can_place_on: BlockPred,
    pub can_replace: BlockPred,
}

impl SpikeConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let provider = StateProvider::parse(env, get(v, "state", ctx)?, ctx)?;
        let state = provider.constant().ok_or_else(|| format!("{ctx}: state must be fixed"))?;
        Ok(Self {
            state,
            can_place_on: BlockPred::parse(env, get(v, "can_place_on", ctx)?, ctx)?,
            can_replace: BlockPred::parse(env, get(v, "can_replace", ctx)?, ctx)?,
        })
    }
}

pub fn place_spike(cfg: &SpikeConfig, level: &mut Level<'_>, rng: &mut Rng, mut origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    while blocks.is_air(level.get(origin.x, origin.y, origin.z)) && origin.y > level.min_y + 2 {
        origin = origin.below();
    }
    if !cfg.can_place_on.test(level, origin.x, origin.y, origin.z) {
        return false;
    }
    origin = origin.offset(0, rng.next_int_bounded(4), 0);
    let height = rng.next_int_bounded(4) + 7;
    let width = height / 4 + rng.next_int_bounded(2);
    if width > 1 && rng.next_int_bounded(60) == 0 {
        origin = origin.offset(0, 10 + rng.next_int_bounded(30), 0);
    }
    let fill = |level: &mut Level<'_>, p: Pos| {
        if blocks.is_air(level.get(p.x, p.y, p.z)) || cfg.can_replace.test(level, p.x, p.y, p.z) {
            level.set(p.x, p.y, p.z, cfg.state);
        }
    };
    for y_off in 0..height {
        let scale = (1.0f32 - y_off as f32 / height as f32) * width as f32;
        let new_width = scale.ceil() as i32;
        for xo in -new_width..=new_width {
            let dx = xo.abs() as f32 - 0.25f32;
            for zo in -new_width..=new_width {
                let dz = zo.abs() as f32 - 0.25f32;
                if ((xo == 0 && zo == 0) || !(dx * dx + dz * dz > scale * scale))
                    && ((xo != -new_width && xo != new_width && zo != -new_width && zo != new_width) || !(rng.next_float() > 0.75f32))
                {
                    fill(level, origin.offset(xo, y_off, zo));
                    if y_off != 0 && new_width > 1 {
                        fill(level, origin.offset(xo, -y_off, zo));
                    }
                }
            }
        }
    }
    let pillar = (width - 1).clamp(0, 1);
    for xo in -pillar..=pillar {
        for zo in -pillar..=pillar {
            let mut cursor = origin.offset(xo, -1, zo);
            let mut run = 50;
            if xo.abs() == 1 && zo.abs() == 1 {
                run = rng.next_int_bounded(5);
            }
            while cursor.y > 50 {
                let s = level.get(cursor.x, cursor.y, cursor.z);
                if !blocks.is_air(s) && !cfg.can_replace.test(level, cursor.x, cursor.y, cursor.z) && s != cfg.state {
                    break;
                }
                level.set(cursor.x, cursor.y, cursor.z, cfg.state);
                cursor = cursor.below();
                run -= 1;
                if run <= 0 {
                    cursor = cursor.offset(0, -(rng.next_int_bounded(5) + 1), 0);
                    run = rng.next_int_bounded(5);
                }
            }
        }
    }
    true
}

pub fn place_blue_ice(level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    if origin.y > level.sea_level - 1 {
        return false;
    }
    let at = |level: &Level<'_>, p: Pos| level.get(p.x, p.y, p.z);
    if !is(env, at(level, origin), "water") && !is(env, at(level, origin.below()), "water") {
        return false;
    }
    let found = Dir::ALL.iter().any(|d| *d != Dir::Down && is(env, at(level, origin.relative(*d)), "packed_ice"));
    if !found {
        return false;
    }
    let blue = state(env, "blue_ice");
    level.set(origin.x, origin.y, origin.z, blue);
    for _ in 0..200 {
        let y_off = rng.next_int_bounded(5) - rng.next_int_bounded(6);
        let mut xz = 3;
        if y_off < 2 {
            xz += y_off / 2;
        }
        if xz >= 1 {
            let dx = rng.next_int_bounded(xz) - rng.next_int_bounded(xz);
            let dz = rng.next_int_bounded(xz) - rng.next_int_bounded(xz);
            let p = origin.offset(dx, y_off, dz);
            let s = at(level, p);
            if blocks.is_air(s) || is(env, s, "water") || is(env, s, "packed_ice") || is(env, s, "ice") {
                for d in Dir::ALL {
                    if is(env, at(level, p.relative(d)), "blue_ice") {
                        level.set(p.x, p.y, p.z, blue);
                        break;
                    }
                }
            }
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct IcebergConfig {
    pub state: State,
}

impl IcebergConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let provider = StateProvider::parse(env, get(v, "state", ctx)?, ctx)?;
        Ok(Self { state: provider.constant().ok_or_else(|| format!("{ctx}: state must be fixed"))? })
    }
}

struct Iceberg<'a, 'l> {
    level: &'a mut Level<'l>,
    rng: &'a mut Rng,
    env: &'l Env,
    snow_block: State,
}

fn pow2(v: f64) -> f64 {
    v * v
}

impl Iceberg<'_, '_> {
    fn get(&self, p: Pos) -> State {
        self.level.get(p.x, p.y, p.z)
    }

    fn put(&mut self, p: Pos, s: State) {
        self.level.set(p.x, p.y, p.z, s);
    }

    fn is_iceberg(&self, s: State) -> bool {
        is(self.env, s, "packed_ice") || s == self.snow_block || is(self.env, s, "blue_ice")
    }

    fn is_snow_block(&self, s: State) -> bool {
        is(self.env, s, "snow_block")
    }

    fn radius_round(&mut self, y_off: i32, height: i32, width: i32) -> i32 {
        let k = 3.5f32 - self.rng.next_float();
        let mut scale = (1.0f32 - (y_off as f32 * y_off as f32) / (height as f32 * k)) * width as f32;
        if height > 15 + self.rng.next_int_bounded(5) {
            let temp = if y_off < 3 + self.rng.next_int_bounded(6) { y_off / 2 } else { y_off };
            scale = (1.0f32 - temp as f32 / (height as f32 * k * 0.4f32)) * width as f32;
        }
        (scale / 2.0f32).ceil() as i32
    }

    fn radius_ellipse(y_off: i32, height: i32, width: i32) -> i32 {
        let scale = (1.0f32 - (y_off as f32 * y_off as f32) / (height as f32 * 1.0f32)) * width as f32;
        (scale / 2.0f32).ceil() as i32
    }

    fn radius_steep(&mut self, y_off: i32, height: i32, width: i32) -> i32 {
        let k = 1.0f32 + self.rng.next_float() / 2.0f32;
        let scale = (1.0f32 - y_off as f32 / (height as f32 * k)) * width as f32;
        (scale / 2.0f32).ceil() as i32
    }

    fn dist_circle(&mut self, xo: i32, zo: i32, radius: i32) -> f64 {
        let f = self.rng.next_float().clamp(0.2f32, 0.8f32);
        let off = 10.0f32 * f / radius as f32;
        f64::from(off) + pow2(f64::from(xo)) + pow2(f64::from(zo)) - pow2(f64::from(radius))
    }

    fn ellipse_c(y_off: i32, height: i32, c: i32) -> i32 {
        let mut c = c;
        if y_off > 0 && height - y_off <= 3 {
            c -= 4 - (height - y_off);
        }
        c
    }

    fn set_block(&mut self, p: Pos, h_diff: i32, height: i32, is_ellipse: bool, snow_top: bool, main: State) {
        let s = self.get(p);
        let blocks = &self.env.blocks;
        if blocks.is_air(s) || self.is_snow_block(s) || is(self.env, s, "ice") || is(self.env, s, "water") {
            let randomness = !is_ellipse || self.rng.next_double() > 0.05;
            let divisor = if is_ellipse { 3 } else { 2 };
            let water = is(self.env, s, "water");
            if snow_top
                && !water
                && f64::from(h_diff) <= f64::from(self.rng.next_int_bounded((height / divisor).max(1))) + f64::from(height) * 0.6
                && randomness
            {
                self.put(p, self.snow_block);
            } else {
                self.put(p, main);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn block(&mut self, origin: Pos, height: i32, xo: i32, y_off: i32, zo: i32, radius: i32, a: i32, is_ellipse: bool, c: i32, angle: f64, snow_top: bool, main: State) {
        let signed = if is_ellipse {
            signed_ellipse(xo, zo, (0, 0), a, Self::ellipse_c(y_off, height, c), angle)
        } else {
            self.dist_circle(xo, zo, radius)
        };
        if signed < 0.0 {
            let p = origin.offset(xo, y_off, zo);
            let compare = if is_ellipse { -0.5 } else { f64::from(-6 - self.rng.next_int_bounded(3)) };
            if signed > compare && self.rng.next_double() > 0.9 {
                return;
            }
            self.set_block(p, height - y_off, height, is_ellipse, snow_top, main);
        }
    }

    fn smooth(&mut self, origin: Pos, width: i32, height: i32, is_ellipse: bool, ellipse_a: i32) {
        let a = if is_ellipse { ellipse_a } else { width / 2 };
        for x in -a..=a {
            for z in -a..=a {
                for y_off in 0..=height {
                    let p = origin.offset(x, y_off, z);
                    let s = self.get(p);
                    if self.is_iceberg(s) || is(self.env, s, "snow") {
                        if self.env.blocks.is_air(self.get(p.below())) {
                            let air = state(self.env, "air");
                            self.put(p, air);
                            self.put(p.above(), air);
                        } else if self.is_iceberg(s) {
                            let sides = [Dir::West, Dir::East, Dir::North, Dir::South];
                            let counter = sides.iter().filter(|d| !self.is_iceberg(self.get(p.relative(**d)))).count();
                            if counter >= 3 {
                                let air = state(self.env, "air");
                                self.put(p, air);
                            }
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn carve(&mut self, radius: i32, y_off: i32, origin: Pos, under_water: bool, angle: f64, local: (i32, i32), ellipse_a: i32, ellipse_c: i32) {
        let a = radius + 1 + ellipse_a / 3;
        let c = (radius - 3).min(3) + ellipse_c / 2 - 1;
        for xo in -a..a {
            for zo in -a..a {
                if signed_ellipse(xo, zo, local, a, c, angle) < 0.0 {
                    let p = origin.offset(xo, y_off, zo);
                    let s = self.get(p);
                    if self.is_iceberg(s) || self.is_snow_block(s) {
                        if under_water {
                            let water = state(self.env, "water");
                            self.put(p, water);
                        } else {
                            let air = state(self.env, "air");
                            self.put(p, air);
                            if is(self.env, self.get(p.above()), "snow") {
                                self.put(p.above(), air);
                            }
                        }
                    }
                }
            }
        }
    }

    fn cut_out(&mut self, width: i32, height: i32, origin: Pos, is_ellipse: bool, ellipse_a: i32, angle_shape: f64, ellipse_c: i32) {
        let sign_x = if self.rng.next_bool() { -1 } else { 1 };
        let sign_z = if self.rng.next_bool() { -1 } else { 1 };
        let mut x_off = self.rng.next_int_bounded((width / 2 - 2).max(1));
        if self.rng.next_bool() {
            x_off = width / 2 + 1 - self.rng.next_int_bounded((width - width / 2 - 1).max(1));
        }
        let mut z_off = self.rng.next_int_bounded((width / 2 - 2).max(1));
        if self.rng.next_bool() {
            z_off = width / 2 + 1 - self.rng.next_int_bounded((width - width / 2 - 1).max(1));
        }
        if is_ellipse {
            x_off = self.rng.next_int_bounded((ellipse_a - 5).max(1));
            z_off = x_off;
        }
        let local = (sign_x * x_off, sign_z * z_off);
        let angle = if is_ellipse { angle_shape + std::f64::consts::FRAC_PI_2 } else { self.rng.next_double() * 2.0 * std::f64::consts::PI };
        for y_off in 0..height - 3 {
            let r = self.radius_round(y_off, height, width);
            self.carve(r, y_off, origin, false, angle, local, ellipse_a, ellipse_c);
        }
        let mut y_off = -1;
        while y_off > -height + self.rng.next_int_bounded(5) {
            let r = self.radius_steep(-y_off, height, width);
            self.carve(r, y_off, origin, true, angle, local, ellipse_a, ellipse_c);
            y_off -= 1;
        }
    }
}

fn signed_ellipse(xo: i32, zo: i32, origin: (i32, i32), a: i32, c: i32, angle: f64) -> f64 {
    let (dx, dz) = (f64::from(xo - origin.0), f64::from(zo - origin.1));
    pow2((dx * angle.cos() - dz * angle.sin()) / f64::from(a)) + pow2((dx * angle.sin() + dz * angle.cos()) / f64::from(c)) - 1.0
}

pub fn place_iceberg(cfg: &IcebergConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let origin = Pos::new(origin.x, level.sea_level, origin.z);
    let mut b = Iceberg { level, rng, env, snow_block: state(env, "snow_block") };
    let snow_top = b.rng.next_double() > 0.7;
    let main = cfg.state;
    let shape_angle = b.rng.next_double() * 2.0 * std::f64::consts::PI;
    let ellipse_a = 11 - b.rng.next_int_bounded(5);
    let ellipse_c = 3 + b.rng.next_int_bounded(3);
    let is_ellipse = b.rng.next_double() > 0.7;
    let mut over = if is_ellipse { b.rng.next_int_bounded(6) + 6 } else { b.rng.next_int_bounded(15) + 3 };
    if !is_ellipse && b.rng.next_double() > 0.9 {
        over += b.rng.next_int_bounded(19) + 7;
    }
    let under = (over + b.rng.next_int_bounded(11)).min(18);
    let width = (over + b.rng.next_int_bounded(7) - b.rng.next_int_bounded(5)).min(11);
    let a = if is_ellipse { ellipse_a } else { 11 };
    for xo in -a..a {
        for zo in -a..a {
            for y_off in 0..over {
                let radius = if is_ellipse { Iceberg::radius_ellipse(y_off, over, width) } else { b.radius_round(y_off, over, width) };
                if is_ellipse || xo < radius {
                    b.block(origin, over, xo, y_off, zo, radius, a, is_ellipse, ellipse_c, shape_angle, snow_top, main);
                }
            }
        }
    }
    b.smooth(origin, width, over, is_ellipse, ellipse_a);
    for xo in -a..a {
        for zo in -a..a {
            let mut y_off = -1;
            while y_off > -under {
                let new_a = if is_ellipse {
                    (a as f32 * (1.0f32 - (y_off as f32 * y_off as f32) / (under as f32 * 8.0f32))).ceil() as i32
                } else {
                    a
                };
                let radius = b.radius_steep(-y_off, under, width);
                if xo < radius {
                    b.block(origin, under, xo, y_off, zo, radius, new_a, is_ellipse, ellipse_c, shape_angle, snow_top, main);
                }
                y_off -= 1;
            }
        }
    }
    let cut = if is_ellipse { b.rng.next_double() > 0.1 } else { b.rng.next_double() > 0.7 };
    if cut {
        b.cut_out(width, over, origin, is_ellipse, ellipse_a, shape_angle, ellipse_c);
    }
    true
}
