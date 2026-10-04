//! The noise-based aquifer: which fluid fills a non-solid cell.
//!
//! The call sequence is part of the result. Every request below the skip height
//! samples the surface level, and the surface-level function is cached per chunk,
//! so which path (bulk, at construction, or scalar, later) fills a cache entry
//! first decides that entry's bits. [`Aquifer::new`] and
//! [`Aquifer::compute_substance`] therefore issue their samples in the order the
//! reference does.

use std::collections::HashMap;

use super::sampler::{Ctx, Program, SId};
use super::volume::Volume;
use crate::rng::{AnyPositionalFactory, PositionalRandomFactory, RandomSource};

/// The fluids a noise-settings document can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fluid {
    Air,
    Water,
    Lava,
}

/// A fluid and the Y below which it fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FluidStatus {
    pub level: i32,
    pub fluid: Fluid,
}

impl FluidStatus {
    pub fn at(&self, y: i32) -> Fluid {
        if y < self.level { self.fluid } else { Fluid::Air }
    }
}

/// The world-wide fluid before aquifers refine it: lava deep down, else the sea.
#[derive(Clone, Copy, Debug)]
pub struct FluidPicker {
    lava: FluidStatus,
    sea: FluidStatus,
    lava_below: i32,
}

impl FluidPicker {
    pub fn new(sea_level: i32, default_fluid: Fluid) -> Self {
        Self {
            lava: FluidStatus { level: -54, fluid: Fluid::Lava },
            sea: FluidStatus { level: sea_level, fluid: default_fluid },
            lava_below: (-54).min(sea_level),
        }
    }

    pub fn compute(&self, _x: i32, y: i32, _z: i32) -> FluidStatus {
        if y < self.lava_below { self.lava } else { self.sea }
    }
}

/// The density functions an aquifer samples.
#[derive(Clone, Copy, Debug)]
pub struct AquiferFunctions {
    pub barrier: SId,
    pub fluid_level_floodedness: SId,
    pub fluid_level_spread: SId,
    pub lava: SId,
    pub exclusion: SId,
    pub surface_level: SId,
}

const WAY_BELOW_MIN_Y: i32 = -32512;

fn grid_x(block: i32) -> i32 {
    block >> 4
}
fn from_grid_x(grid: i32, offset: i32) -> i32 {
    (grid << 4).wrapping_add(offset)
}
fn grid_y(block: i32) -> i32 {
    block.div_euclid(12)
}
fn from_grid_y(grid: i32, offset: i32) -> i32 {
    grid.wrapping_mul(12).wrapping_add(offset)
}

fn pack(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38) | ((i64::from(z) & 0x3FF_FFFF) << 12) | (i64::from(y) & 0xFFF)
}
fn unpack_x(l: i64) -> i32 {
    (l >> 38) as i32
}
fn unpack_y(l: i64) -> i32 {
    ((l << 52) >> 52) as i32
}
fn unpack_z(l: i64) -> i32 {
    ((l << 26) >> 38) as i32
}

fn chunk_pack(x: i32, z: i32) -> i64 {
    (i64::from(x) & 0xFFFF_FFFF) | ((i64::from(z) & 0xFFFF_FFFF) << 32)
}

fn similarity(d1: i32, d2: i32) -> f64 {
    1.0 - f64::from(d2.wrapping_sub(d1)) / 25.0
}

const SURFACE_OFFSETS: [[i32; 2]; 13] =
    [[0, 0], [-2, -1], [-1, -1], [0, -1], [1, -1], [-3, 0], [-2, 0], [-1, 0], [1, 0], [-2, 1], [-1, 1], [0, 1], [1, 1]];

pub struct Aquifer {
    functions: AquiferFunctions,
    random: AnyPositionalFactory,
    picker: FluidPicker,
    status_cache: Vec<Option<FluidStatus>>,
    location_cache: Vec<i64>,
    surface_cache: HashMap<i64, i32>,
    should_schedule: bool,
    skip_sampling_above_y: i32,
    min_grid: [i32; 3],
    grid_size_x: i32,
    grid_size_z: i32,
}

impl Aquifer {
    /// Builds the per-chunk state. Samples the surface level over the whole
    /// aquifer footprint once, as a volume, before any scalar request.
    pub fn new(
        program: &Program,
        ctx: &mut Ctx,
        functions: AquiferFunctions,
        random: AnyPositionalFactory,
        volume: &Volume,
        picker: FluidPicker,
    ) -> Self {
        let min_grid_x = grid_x(volume.min[0] - 5);
        let max_grid_x = grid_x(volume.max_block(0) - 5) + 1;
        let grid_size_x = max_grid_x - min_grid_x + 1;
        let min_grid_y = grid_y(volume.min[1] + 1) - 1;
        let max_grid_y = grid_y(volume.max_block(1) + 1) + 1;
        let grid_size_y = max_grid_y - min_grid_y + 1;
        let min_grid_z = grid_x(volume.min[2] - 5);
        let max_grid_z = grid_x(volume.max_block(2) - 5) + 1;
        let grid_size_z = max_grid_z - min_grid_z + 1;
        let total = (grid_size_x * grid_size_y * grid_size_z) as usize;
        let mut a = Self {
            functions,
            random,
            picker,
            status_cache: vec![None; total],
            location_cache: vec![i64::MAX; total],
            surface_cache: HashMap::new(),
            should_schedule: false,
            skip_sampling_above_y: 0,
            min_grid: [min_grid_x, min_grid_y, min_grid_z],
            grid_size_x,
            grid_size_z,
        };
        let max_surface = a.max_surface_level(
            program,
            ctx,
            from_grid_x(min_grid_x, 0),
            from_grid_x(min_grid_z, 0),
            from_grid_x(max_grid_x, 9),
            from_grid_x(max_grid_z, 9),
        );
        let max_adjusted = max_surface.wrapping_add(8);
        let skip_grid_y = grid_y(max_adjusted + 12) + 1;
        a.skip_sampling_above_y = from_grid_y(skip_grid_y, 11) - 1;
        a
    }

    pub fn should_schedule_fluid_update(&self) -> bool {
        self.should_schedule
    }

    fn surface_level(&mut self, program: &Program, ctx: &mut Ctx, x: i32, z: i32) -> i32 {
        let qx = (x >> 2) << 2;
        let qz = (z >> 2) << 2;
        let key = chunk_pack(qx, qz);
        if let Some(&v) = self.surface_cache.get(&key) {
            return v;
        }
        let v = (f64::from(program.value(ctx, self.functions.surface_level, qx, 0, qz)).floor()) as i32;
        self.surface_cache.insert(key, v);
        v
    }

    fn max_surface_level(&mut self, program: &Program, ctx: &mut Ctx, min_x: i32, min_z: i32, max_x: i32, max_z: i32) -> i32 {
        let (min_qx, max_qx) = (min_x >> 2, max_x >> 2);
        let (min_qz, max_qz) = (min_z >> 2, max_z >> 2);
        let volume = Volume::new([max_qx - min_qx + 1, 1, max_qz - min_qz + 1], [min_qx << 2, 0, min_qz << 2], [4, 1, 4]);
        let mut buffer = vec![0.0f32; volume.len()];
        program.volume(ctx, self.functions.surface_level, &mut buffer, &volume);
        let mut max_y = i32::MIN;
        for z in 0..volume.size[2] {
            for x in 0..volume.size[0] {
                let level = (f64::from(buffer[volume.index(x, 0, z)]).floor()) as i32;
                self.surface_cache.insert(chunk_pack(volume.block_x(x), volume.block_z(z)), level);
                if level > max_y {
                    max_y = level;
                }
            }
        }
        max_y
    }

    fn index(&self, gx: i32, gy: i32, gz: i32) -> usize {
        let x = gx - self.min_grid[0];
        let y = gy - self.min_grid[1];
        let z = gz - self.min_grid[2];
        ((y * self.grid_size_z + z) * self.grid_size_x + x) as usize
    }

    /// The fluid occupying a non-solid cell, or `None` when the cell stays solid.
    /// `density` is the final-density value widened to `f64`.
    pub fn compute_substance(&mut self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32, density: f64) -> Option<Fluid> {
        if density > 0.0 {
            self.should_schedule = false;
            return None;
        }
        let global = self.picker.compute(x, y, z);
        if y > self.skip_sampling_above_y {
            self.should_schedule = false;
            return Some(global.at(y));
        }
        if global.at(y) == Fluid::Lava {
            self.should_schedule = false;
            return Some(Fluid::Lava);
        }
        let x_anchor = grid_x(x - 5);
        let y_anchor = grid_y(y + 1);
        let z_anchor = grid_x(z - 5);
        let mut dist = [i32::MAX; 4];
        let mut closest = [0usize; 4];
        for x1 in 0..=1 {
            for y1 in -1..=1 {
                for z1 in 0..=1 {
                    let (sgx, sgy, sgz) = (x_anchor + x1, y_anchor + y1, z_anchor + z1);
                    let index = self.index(sgx, sgy, sgz);
                    let existing = self.location_cache[index];
                    let location = if existing != i64::MAX {
                        existing
                    } else {
                        let mut random = self.random.at(sgx, sgy, sgz);
                        let lx = from_grid_x(sgx, random.next_int_bounded(10));
                        let ly = from_grid_y(sgy, random.next_int_bounded(9));
                        let lz = from_grid_x(sgz, random.next_int_bounded(10));
                        let l = pack(lx, ly, lz);
                        self.location_cache[index] = l;
                        l
                    };
                    let dx = unpack_x(location).wrapping_sub(x);
                    let dy = unpack_y(location).wrapping_sub(y);
                    let dz = unpack_z(location).wrapping_sub(z);
                    let d = dx.wrapping_mul(dx).wrapping_add(dy.wrapping_mul(dy)).wrapping_add(dz.wrapping_mul(dz));
                    if dist[0] >= d {
                        closest = [index, closest[0], closest[1], closest[2]];
                        dist = [d, dist[0], dist[1], dist[2]];
                    } else if dist[1] >= d {
                        closest = [closest[0], index, closest[1], closest[2]];
                        dist = [dist[0], d, dist[1], dist[2]];
                    } else if dist[2] >= d {
                        closest = [closest[0], closest[1], index, closest[2]];
                        dist = [dist[0], dist[1], d, dist[2]];
                    } else if dist[3] >= d {
                        closest[3] = index;
                        dist[3] = d;
                    }
                }
            }
        }
        let status1 = self.status(program, ctx, closest[0]);
        let similarity12 = similarity(dist[0], dist[1]);
        let fluid = status1.at(y);
        if similarity12 <= 0.0 {
            if similarity12 >= FLOWING_UPDATE_SIMILARITY {
                let status2 = self.status(program, ctx, closest[1]);
                self.should_schedule = status1 != status2;
            } else {
                self.should_schedule = false;
            }
            return Some(fluid);
        }
        if fluid == Fluid::Water && self.picker.compute(x, y - 1, z).at(y - 1) == Fluid::Lava {
            self.should_schedule = true;
            return Some(fluid);
        }
        let mut barrier_value = f64::NAN;
        let status2 = self.status(program, ctx, closest[1]);
        let barrier12 = similarity12 * self.pressure(program, ctx, x, y, z, &mut barrier_value, status1, status2);
        if density + barrier12 > 0.0 {
            self.should_schedule = false;
            return None;
        }
        let status3 = self.status(program, ctx, closest[2]);
        let similarity13 = similarity(dist[0], dist[2]);
        if similarity13 > 0.0 {
            let barrier13 = similarity12 * similarity13 * self.pressure(program, ctx, x, y, z, &mut barrier_value, status1, status3);
            if density + barrier13 > 0.0 {
                self.should_schedule = false;
                return None;
            }
        }
        let similarity23 = similarity(dist[1], dist[2]);
        if similarity23 > 0.0 {
            let barrier23 = similarity12 * similarity23 * self.pressure(program, ctx, x, y, z, &mut barrier_value, status2, status3);
            if density + barrier23 > 0.0 {
                self.should_schedule = false;
                return None;
            }
        }
        let may_flow12 = status1 != status2;
        let may_flow23 = similarity23 >= FLOWING_UPDATE_SIMILARITY && status2 != status3;
        let may_flow13 = similarity13 >= FLOWING_UPDATE_SIMILARITY && status1 != status3;
        if !may_flow12 && !may_flow23 && !may_flow13 {
            self.should_schedule = similarity13 >= FLOWING_UPDATE_SIMILARITY
                && similarity(dist[0], dist[3]) >= FLOWING_UPDATE_SIMILARITY
                && status1 != self.status(program, ctx, closest[3]);
        } else {
            self.should_schedule = true;
        }
        Some(fluid)
    }

    #[allow(clippy::too_many_arguments)]
    fn pressure(&self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32, barrier_value: &mut f64, s1: FluidStatus, s2: FluidStatus) -> f64 {
        let (t1, t2) = (s1.at(y), s2.at(y));
        if (t1 == Fluid::Lava && t2 == Fluid::Water) || (t1 == Fluid::Water && t2 == Fluid::Lava) {
            return 2.0;
        }
        let fluid_y_diff = (s1.level - s2.level).abs();
        if fluid_y_diff == 0 {
            return 0.0;
        }
        let average_fluid_y = 0.5 * f64::from(s1.level + s2.level);
        let how_far_above = f64::from(y) + 0.5 - average_fluid_y;
        let base_value = f64::from(fluid_y_diff) / 2.0;
        let distance_from_edge = base_value - how_far_above.abs();
        let gradient = if how_far_above > 0.0 {
            let center = 0.0 + distance_from_edge;
            if center > 0.0 { center / 1.5 } else { center / 2.5 }
        } else {
            let center = 3.0 + distance_from_edge;
            if center > 0.0 { center / 3.0 } else { center / 10.0 }
        };
        let noise_value = if !(gradient < -2.0) && !(gradient > 2.0) {
            if barrier_value.is_nan() {
                let n = f64::from(program.value(ctx, self.functions.barrier, x, y, z));
                *barrier_value = n;
                n
            } else {
                *barrier_value
            }
        } else {
            0.0
        };
        2.0 * (noise_value + gradient)
    }

    fn status(&mut self, program: &Program, ctx: &mut Ctx, index: usize) -> FluidStatus {
        if let Some(s) = self.status_cache[index] {
            return s;
        }
        let l = self.location_cache[index];
        let s = self.compute_fluid(program, ctx, unpack_x(l), unpack_y(l), unpack_z(l));
        self.status_cache[index] = Some(s);
        s
    }

    fn compute_fluid(&mut self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32) -> FluidStatus {
        let global = self.picker.compute(x, y, z);
        let mut lowest = i32::MAX;
        let top = y + 12;
        let bottom = y - 12;
        let mut center_under_global = false;
        for offset in SURFACE_OFFSETS {
            let sx = x + offset[0] * 16;
            let sz = z + offset[1] * 16;
            let surface = self.surface_level(program, ctx, sx, sz);
            let adjusted = surface + 8;
            let start = offset[0] == 0 && offset[1] == 0;
            if start && bottom > adjusted {
                return global;
            }
            let top_pokes_above = top > adjusted;
            if top_pokes_above || start {
                let at_surface = self.picker.compute(sx, adjusted, sz);
                if at_surface.at(adjusted) != Fluid::Air {
                    if start {
                        center_under_global = true;
                    }
                    if top_pokes_above {
                        return at_surface;
                    }
                }
            }
            lowest = lowest.min(surface);
        }
        let level = self.compute_surface_level(program, ctx, x, y, z, global, lowest, center_under_global);
        FluidStatus { level, fluid: self.fluid_type(program, ctx, x, y, z, global, level) }
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_surface_level(&self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32, global: FluidStatus, lowest: i32, center_under_global: bool) -> i32 {
        let (partially, fully);
        if program.value(ctx, self.functions.exclusion, x, y, z) > 0.0 {
            partially = -1.0;
            fully = -1.0;
        } else {
            let distance_below = (lowest + 8) - y;
            let factor = if center_under_global { clamped_map(f64::from(distance_below), 0.0, 64.0, 1.0, 0.0) } else { 0.0 };
            let noise = clamp_d(f64::from(program.value(ctx, self.functions.fluid_level_floodedness, x, y, z)), -1.0, 1.0);
            let fully_threshold = map(factor, 1.0, 0.0, -0.3, 0.8);
            let partial_threshold = map(factor, 1.0, 0.0, -0.8, 0.4);
            partially = noise - partial_threshold;
            fully = noise - fully_threshold;
        }
        if fully > 0.0 {
            global.level
        } else if partially > 0.0 {
            self.randomized_level(program, ctx, x, y, z, lowest)
        } else {
            WAY_BELOW_MIN_Y
        }
    }

    fn randomized_level(&self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32, lowest: i32) -> i32 {
        let cell_x = x.div_euclid(16);
        let cell_y = y.div_euclid(40);
        let cell_z = z.div_euclid(16);
        let middle_y = cell_y * 40 + 20;
        let spread = f64::from(program.value(ctx, self.functions.fluid_level_spread, cell_x, cell_y, cell_z) * 10.0f32);
        let quantized = ((spread / 3.0).floor() as i32).wrapping_mul(3);
        lowest.min(middle_y + quantized)
    }

    #[allow(clippy::too_many_arguments)]
    fn fluid_type(&self, program: &Program, ctx: &mut Ctx, x: i32, y: i32, z: i32, global: FluidStatus, level: i32) -> Fluid {
        let mut fluid = global.fluid;
        if level <= -10 && level != WAY_BELOW_MIN_Y && global.fluid != Fluid::Lava {
            let cx = x.div_euclid(64);
            let cy = y.div_euclid(40);
            let cz = z.div_euclid(64);
            let lava = f64::from(program.value(ctx, self.functions.lava, cx, cy, cz));
            if lava.abs() > 0.3 {
                fluid = Fluid::Lava;
            }
        }
        fluid
    }
}

const FLOWING_UPDATE_SIMILARITY: f64 = {
    // similarity(10 * 10, 12 * 12), evaluated at compile time.
    1.0 - ((144 - 100) as f64) / 25.0
};

pub(crate) fn lerp_d(alpha: f64, p0: f64, p1: f64) -> f64 {
    p0 + alpha * (p1 - p0)
}

pub(crate) fn inverse_lerp(value: f64, min: f64, max: f64) -> f64 {
    (value - min) / (max - min)
}

pub(crate) fn map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    lerp_d(inverse_lerp(value, from_min, from_max), to_min, to_max)
}

fn clamped_map(value: f64, from_min: f64, from_max: f64, to_min: f64, to_max: f64) -> f64 {
    let f = inverse_lerp(value, from_min, from_max);
    if f < 0.0 {
        to_min
    } else if f > 1.0 {
        to_max
    } else {
        lerp_d(f, to_min, to_max)
    }
}

fn clamp_d(value: f64, min: f64, max: f64) -> f64 {
    if value < min {
        min
    } else if value <= max || value.is_nan() {
        if value.is_nan() { value } else { value }
    } else {
        max
    }
}

impl std::fmt::Debug for Aquifer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Aquifer").finish_non_exhaustive()
    }
}
