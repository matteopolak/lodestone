//! 32-bit gradient noise for the 26.3 density system.
//!
//! The scalar and the bulk (`add_to_volume`) paths of one noise do not round
//! identically: the bulk path groups the gradient dot products by axis and
//! reuses them across a run of Y samples. A density graph that mixes both paths
//! (and the cell caches decide which path serves a given request) is only
//! seed-exact if each path keeps its own association, so the two are separate
//! functions and are never "simplified" into one.

use super::volume::Volume;
use crate::rng::{PositionalRandomFactory, RandomSource};

const GRADIENT: [[i32; 3]; 16] = [
    [1, 1, 0], [-1, 1, 0], [1, -1, 0], [-1, -1, 0],
    [1, 0, 1], [-1, 0, 1], [1, 0, -1], [-1, 0, -1],
    [0, 1, 1], [0, -1, 1], [0, 1, -1], [0, -1, -1],
    [1, 1, 0], [0, -1, 1], [-1, 1, 0], [0, -1, -1],
];

/// The largest `f64` below `1.6777216E7`: inputs inside `[-this, this)` are not wrapped.
const HALF_ROUND_OFF: f64 = 16_777_215.999_999_998;
const ROUND_OFF_PERIOD: f64 = 3.355_443_2E7;

fn wrap(x: f64) -> f64 {
    if x >= -HALF_ROUND_OFF && x < HALF_ROUND_OFF {
        x
    } else {
        x - (x / ROUND_OFF_PERIOD + 0.5).floor() * ROUND_OFF_PERIOD
    }
}

#[inline]
fn floor_i32(v: f64) -> i32 {
    v.floor() as i32
}

#[inline]
fn smoothstep(x: f32) -> f32 {
    x * x * x * (x * (x * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(alpha: f32, p0: f32, p1: f32) -> f32 {
    p0 + alpha * (p1 - p0)
}

#[inline]
fn lerp2(a1: f32, a2: f32, x00: f32, x10: f32, x01: f32, x11: f32) -> f32 {
    lerp(a2, lerp(a1, x00, x10), lerp(a1, x01, x11))
}

#[allow(clippy::too_many_arguments)]
#[inline]
fn lerp3(a1: f32, a2: f32, a3: f32, x000: f32, x100: f32, x010: f32, x110: f32, x001: f32, x101: f32, x011: f32, x111: f32) -> f32 {
    lerp(a3, lerp2(a1, a2, x000, x100, x010, x110), lerp2(a1, a2, x001, x101, x011, x111))
}

#[inline]
fn grad_dot(hash: i32, x: f32, y: f32, z: f32) -> f32 {
    let g = GRADIENT[(hash & 15) as usize];
    g[0] as f32 * x + g[1] as f32 * y + g[2] as f32 * z
}

/// One octave of improved Perlin noise.
#[derive(Clone, Debug)]
pub struct Perlin {
    perms: [u8; 256],
    offset: [f64; 3],
}

impl Perlin {
    /// Draws the three offsets and shuffles the permutation, in that order.
    pub fn new<R: RandomSource>(random: &mut R) -> Self {
        Self::with_offset_scale(random, 256.0)
    }

    pub fn with_offset_scale<R: RandomSource>(random: &mut R, scale: f64) -> Self {
        let ox = random.next_double() * scale;
        let oy = random.next_double() * scale;
        let oz = random.next_double() * scale;
        let mut perms = [0u8; 256];
        for (i, p) in perms.iter_mut().enumerate() {
            *p = i as u8;
        }
        for i in 0..256 {
            let offset = random.next_int_bounded(256 - i as i32) as usize;
            perms.swap(i, offset + i);
        }
        Self { perms, offset: [ox, oy, oz] }
    }

    #[inline]
    fn permute(&self, x: i32) -> i32 {
        i32::from(self.perms[(x & 0xFF) as usize])
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_and_lerp(&self, x: i32, y: i32, z: i32, rx: f32, ry: f32, rz: f32, original_ry: f32) -> f32 {
        let x0 = self.permute(x);
        let x1 = self.permute(x.wrapping_add(1));
        let xy00 = self.permute(x0.wrapping_add(y));
        let xy01 = self.permute(x0.wrapping_add(y).wrapping_add(1));
        let xy10 = self.permute(x1.wrapping_add(y));
        let xy11 = self.permute(x1.wrapping_add(y).wrapping_add(1));
        let z1 = z.wrapping_add(1);
        let d000 = grad_dot(self.permute(xy00.wrapping_add(z)), rx, ry, rz);
        let d100 = grad_dot(self.permute(xy10.wrapping_add(z)), rx - 1.0, ry, rz);
        let d010 = grad_dot(self.permute(xy01.wrapping_add(z)), rx, ry - 1.0, rz);
        let d110 = grad_dot(self.permute(xy11.wrapping_add(z)), rx - 1.0, ry - 1.0, rz);
        let d001 = grad_dot(self.permute(xy00.wrapping_add(z1)), rx, ry, rz - 1.0);
        let d101 = grad_dot(self.permute(xy10.wrapping_add(z1)), rx - 1.0, ry, rz - 1.0);
        let d011 = grad_dot(self.permute(xy01.wrapping_add(z1)), rx, ry - 1.0, rz - 1.0);
        let d111 = grad_dot(self.permute(xy11.wrapping_add(z1)), rx - 1.0, ry - 1.0, rz - 1.0);
        let ax = smoothstep(rx);
        let ay = smoothstep(original_ry);
        let az = smoothstep(rz);
        lerp3(ax, ay, az, d000, d100, d010, d110, d001, d101, d011, d111)
    }

    /// Scalar sample.
    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let x = wrap(x) + self.offset[0];
        let y = wrap(y) + self.offset[1];
        let z = wrap(z) + self.offset[2];
        let (fx, fy, fz) = (floor_i32(x), floor_i32(y), floor_i32(z));
        let rx = (x - f64::from(fx)) as f32;
        let ry = (y - f64::from(fy)) as f32;
        let rz = (z - f64::from(fz)) as f32;
        self.sample_and_lerp(fx, fy, fz, rx, ry, rz, ry)
    }

    /// Scalar sample with the vertical "smear" used by the legacy terrain noise.
    fn get_smeared(&self, ox: f64, oy: f64, oz: f64, fudge_scale: f64) -> f32 {
        let x = wrap(ox) + self.offset[0];
        let y = wrap(oy) + self.offset[1];
        let z = wrap(oz) + self.offset[2];
        let (fx, fy, fz) = (floor_i32(x), floor_i32(y), floor_i32(z));
        let rx = (x - f64::from(fx)) as f32;
        let ry = y - f64::from(fy);
        let rz = (z - f64::from(fz)) as f32;
        let fudged = (ry - fudge_y(oy, ry, fudge_scale)) as f32;
        self.sample_and_lerp(fx, fy, fz, rx, fudged, rz, ry as f32)
    }

    /// Bulk accumulation, `buffer[i] += amplitude * noise(i)` over a volume.
    ///
    /// `fudge_scale` is `Some` for the smeared variant. The dot products are split
    /// into an XZ part (cached while the Y cell is unchanged) and a Y part.
    pub fn add_to_volume(&self, buffer: &mut [f32], volume: &Volume, xz_scale: f64, y_scale: f64, amplitude: f32, fudge_scale: Option<f64>) {
        let mut d_xz = [0.0f32; 8];
        let mut g_y = [0.0f32; 8];
        let mut index = 0usize;
        for iz in 0..volume.size[2] {
            let z = wrap(f64::from(volume.block_z(iz)) * xz_scale) + self.offset[2];
            let floor_z = floor_i32(z);
            let rz = (z - f64::from(floor_z)) as f32;
            let alpha_z = smoothstep(rz);
            for ix in 0..volume.size[0] {
                let x = wrap(f64::from(volume.block_x(ix)) * xz_scale) + self.offset[0];
                let floor_x = floor_i32(x);
                let rx = (x - f64::from(floor_x)) as f32;
                let x0 = self.permute(floor_x);
                let x1 = self.permute(floor_x.wrapping_add(1));
                let alpha_x = smoothstep(rx);
                let mut last_floor_y = i32::MIN;
                for iy in 0..volume.size[1] {
                    let original_y = f64::from(volume.block_y(iy)) * y_scale;
                    let y = wrap(original_y) + self.offset[1];
                    let floor_y = floor_i32(y);
                    let ry = y - f64::from(floor_y);
                    let alpha_y = smoothstep(ry as f32);
                    if last_floor_y != floor_y {
                        let xy00 = self.permute(x0.wrapping_add(floor_y));
                        let xy01 = self.permute(x0.wrapping_add(floor_y).wrapping_add(1));
                        let xy10 = self.permute(x1.wrapping_add(floor_y));
                        let xy11 = self.permute(x1.wrapping_add(floor_y).wrapping_add(1));
                        let fz1 = floor_z.wrapping_add(1);
                        // Corner order: 000 100 010 110 001 101 011 111.
                        let corners = [
                            (xy00.wrapping_add(floor_z), rx, rz),
                            (xy10.wrapping_add(floor_z), rx - 1.0, rz),
                            (xy01.wrapping_add(floor_z), rx, rz),
                            (xy11.wrapping_add(floor_z), rx - 1.0, rz),
                            (xy00.wrapping_add(fz1), rx, rz - 1.0),
                            (xy10.wrapping_add(fz1), rx - 1.0, rz - 1.0),
                            (xy01.wrapping_add(fz1), rx, rz - 1.0),
                            (xy11.wrapping_add(fz1), rx - 1.0, rz - 1.0),
                        ];
                        for (k, (hash, cx, cz)) in corners.into_iter().enumerate() {
                            let g = GRADIENT[(self.permute(hash) & 15) as usize];
                            d_xz[k] = g[0] as f32 * cx + g[2] as f32 * cz;
                            g_y[k] = g[1] as f32;
                        }
                        last_floor_y = floor_y;
                    }
                    let ry_f = match fudge_scale {
                        Some(scale) => (ry - fudge_y(original_y, ry, scale)) as f32,
                        None => ry as f32,
                    };
                    let value = lerp3(
                        alpha_x, alpha_y, alpha_z,
                        d_xz[0] + g_y[0] * ry_f,
                        d_xz[1] + g_y[1] * ry_f,
                        d_xz[2] + g_y[2] * (ry_f - 1.0),
                        d_xz[3] + g_y[3] * (ry_f - 1.0),
                        d_xz[4] + g_y[4] * ry_f,
                        d_xz[5] + g_y[5] * ry_f,
                        d_xz[6] + g_y[6] * (ry_f - 1.0),
                        d_xz[7] + g_y[7] * (ry_f - 1.0),
                    );
                    buffer[index] += amplitude * value;
                    index += 1;
                }
            }
        }
    }
}

fn fudge_y(original_y: f64, relative_y: f64, fudge_scale: f64) -> f64 {
    let limit = if original_y >= 0.0 && original_y < relative_y { original_y } else { relative_y };
    f64::from(floor_i32(limit / fudge_scale + f64::from(1.0e-7_f32))) * fudge_scale
}

/// A single octave inside a stack.
#[derive(Clone, Debug)]
pub enum Octave {
    Perlin(Perlin),
    /// Perlin whose Y fraction is quantised by the given vertical scale.
    Smeared(Perlin, f64),
}

impl Octave {
    fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        match self {
            Self::Perlin(p) => p.get(x, y, z),
            Self::Smeared(p, scale) => p.get_smeared(x, y, z, *scale),
        }
    }

    fn add_to_volume(&self, buffer: &mut [f32], volume: &Volume, xz: f64, y: f64, amplitude: f32) {
        match self {
            Self::Perlin(p) => p.add_to_volume(buffer, volume, xz, y, amplitude, None),
            Self::Smeared(p, scale) => p.add_to_volume(buffer, volume, xz, y, amplitude, Some(*scale)),
        }
    }
}

#[derive(Clone, Debug)]
struct Layer {
    octave: Octave,
    frequency: f64,
    amplitude: f32,
}

/// A weighted sum of octaves, each at its own frequency.
#[derive(Clone, Debug, Default)]
pub struct NoiseStack {
    layers: Vec<Layer>,
}

impl NoiseStack {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, octave: Octave, frequency: f64, amplitude: f32) {
        self.layers.push(Layer { octave, frequency, amplitude });
    }

    /// Appends another stack's layers with scaled frequency and amplitude.
    pub fn add_stack(&mut self, other: &Self, frequency: f64, amplitude: f32) {
        for layer in &other.layers {
            self.layers.push(Layer {
                octave: layer.octave.clone(),
                frequency: layer.frequency * frequency,
                amplitude: layer.amplitude * amplitude,
            });
        }
    }

    pub fn get(&self, x: f64, y: f64, z: f64) -> f32 {
        let mut value = 0.0f32;
        for layer in &self.layers {
            let f = layer.frequency;
            value += layer.amplitude * layer.octave.get(x * f, y * f, z * f);
        }
        value
    }

    pub fn add_to_volume(&self, buffer: &mut [f32], volume: &Volume, xz: f64, y: f64, amplitude: f32) {
        for layer in &self.layers {
            let f = layer.frequency;
            layer.octave.add_to_volume(buffer, volume, xz * f, y * f, amplitude * layer.amplitude);
        }
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }
}

/// How a noise's amplitudes are normalised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalization {
    Disabled,
    Enabled,
    Legacy,
}

/// The `noise/*.json` document.
#[derive(Clone, Debug, PartialEq)]
pub struct NoiseParams {
    pub base_amplitude: f64,
    pub base_octave: i32,
    pub octave_count: i32,
    pub normalize: Normalization,
    pub amplitude_modifiers: Vec<f64>,
}

#[derive(Clone, Copy, Debug)]
struct OctaveInfo {
    index: i32,
    frequency: f64,
    amplitude: f64,
}

/// Derived constants of a [`NoiseParams`]: the octave table, the normalisation
/// factor and the value range a graph's compile-time shortcuts rely on.
#[derive(Clone, Debug)]
pub struct NormalNoise {
    params: NoiseParams,
    octaves: Vec<OctaveInfo>,
    normalization_factor: f64,
    range: super::interval::Interval,
}

const SECOND_SAMPLE_FACTOR: f64 = 1.018_126_888_217_522_7;
const TARGET_DEVIATION: f64 = 0.333_333_333_333_333_3;
const PERLIN_DEVIATION: f64 = 0.270_224_783_124_521_1;

fn amplitude_modifier(modifiers: &[f64], index: usize) -> f64 {
    if modifiers.is_empty() { 1.0 } else { modifiers[index] }
}

fn build_octaves(base_octave: i32, base_amplitude: f64, octave_count: i32, normalize: bool, modifiers: &[f64]) -> Vec<OctaveInfo> {
    let mut frequency = 2.0f64.powi(base_octave);
    let mut amplitude = base_amplitude;
    if normalize {
        amplitude *= 0.5f64.powi(-(octave_count - 1)) / (0.5f64.powi(-octave_count) - 1.0);
    }
    let mut out = Vec::with_capacity(octave_count as usize);
    for i in 0..octave_count {
        let modifier = amplitude_modifier(modifiers, i as usize);
        if modifier != 0.0 {
            out.push(OctaveInfo { index: base_octave + i, frequency, amplitude: amplitude * modifier });
        }
        frequency *= 2.0;
        amplitude *= 0.5;
    }
    out
}

/// `DoubleStream.sum`: Kahan summation, finished by subtracting the compensation.
fn compensated_sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut sum, mut compensation, mut simple) = (0.0f64, 0.0f64, 0.0f64);
    for value in values {
        let tmp = value - compensation;
        let velvel = sum + tmp;
        compensation = (velvel - sum) - tmp;
        sum = velvel;
        simple += value;
    }
    let total = sum - compensation;
    if total.is_nan() && simple.is_infinite() { simple } else { total }
}

fn estimate_deviation(octaves: &[OctaveInfo]) -> f64 {
    let mut variance = 0.0;
    for o in octaves {
        let layer = PERLIN_DEVIATION * o.amplitude.abs();
        variance += layer * layer;
    }
    variance.sqrt()
}

fn compute_normalization_factor(target_amplitude: f64, octaves: &[OctaveInfo]) -> f64 {
    let input_deviation = estimate_deviation(octaves);
    if input_deviation == 0.0 {
        return 0.0;
    }
    let input_sum_deviation = input_deviation * 2.0f64.sqrt();
    let target_deviation = target_amplitude * TARGET_DEVIATION;
    target_deviation / input_sum_deviation
}

fn parity_normalization_factor(base_amplitude: f64, octave_count: i32, modifiers: &[f64]) -> f64 {
    let mut min_octave = i32::MAX;
    let mut max_octave = i32::MIN;
    for i in 0..octave_count {
        if amplitude_modifier(modifiers, i as usize) != 0.0 {
            min_octave = min_octave.min(i);
            max_octave = max_octave.max(i);
        }
    }
    let span = max_octave.wrapping_sub(min_octave);
    base_amplitude * 0.5 * TARGET_DEVIATION / (0.1 * (1.0 + 1.0 / f64::from(span.wrapping_add(1))))
}

impl NormalNoise {
    pub fn new(params: NoiseParams) -> Self {
        let octaves = build_octaves(
            params.base_octave, params.base_amplitude, params.octave_count,
            params.normalize != Normalization::Disabled, &params.amplitude_modifiers,
        );
        let mut target_amplitude = compensated_sum(octaves.iter().map(|o| o.amplitude.abs()));
        let mut normalization_factor = compute_normalization_factor(target_amplitude, &octaves);
        if params.normalize == Normalization::Legacy && normalization_factor != 0.0 {
            let parity = parity_normalization_factor(params.base_amplitude, params.octave_count, &params.amplitude_modifiers);
            target_amplitude *= parity / normalization_factor;
            normalization_factor = parity;
        }
        let range = super::interval::Interval::symmetric((target_amplitude * TARGET_DEVIATION * 6.0) as f32);
        Self { params, octaves, normalization_factor, range }
    }

    pub fn range(&self) -> super::interval::Interval {
        self.range
    }

    pub fn params(&self) -> &NoiseParams {
        &self.params
    }

    /// Builds the two-sample stack from a random source forked per sample.
    pub fn create<R: RandomSource>(&self, random: &mut R) -> NoiseStack {
        let first_factory = random.fork_positional();
        let second_factory = random.fork_positional();
        let mut stack = NoiseStack::new();
        for octave in &self.octaves {
            let seed = format!("octave_{}", octave.index);
            let first = Perlin::new(&mut first_factory.from_hash_of(&seed));
            let second = Perlin::new(&mut second_factory.from_hash_of(&seed));
            let value_factor = (self.normalization_factor * octave.amplitude) as f32;
            stack.add(Octave::Perlin(first), octave.frequency, value_factor);
            stack.add(Octave::Perlin(second), octave.frequency * SECOND_SAMPLE_FACTOR, value_factor);
        }
        stack
    }

    /// The pre-1.18 nether climate noise: sequentially seeded octaves, the lower
    /// ones skipped by draw count when their amplitude is zero.
    pub fn create_for_legacy_nether_biome<R: RandomSource>(&self, random: &mut R) -> NoiseStack {
        let amplitudes: Vec<f64> = if self.params.amplitude_modifiers.is_empty() {
            vec![1.0; self.params.octave_count as usize]
        } else {
            self.params.amplitude_modifiers.clone()
        };
        let first = legacy_fbm(random, self.params.base_octave, &amplitudes);
        let second = legacy_fbm(random, self.params.base_octave, &amplitudes);
        let value_factor = (self.normalization_factor * self.params.base_amplitude) as f32;
        let mut stack = NoiseStack::new();
        stack.add_stack(&first, 1.0, value_factor);
        stack.add_stack(&second, SECOND_SAMPLE_FACTOR, value_factor);
        stack
    }
}

fn legacy_fbm<R: RandomSource>(random: &mut R, first_octave: i32, amplitudes: &[f64]) -> NoiseStack {
    let octaves = amplitudes.len() as i32;
    let zero_index = -first_octave;
    let mut levels: Vec<Option<Perlin>> = (0..octaves).map(|_| None).collect();
    let zero_octave = Perlin::new(random);
    if (0..octaves).contains(&zero_index) && amplitudes[zero_index as usize] != 0.0 {
        levels[zero_index as usize] = Some(zero_octave);
    }
    let mut i = zero_index - 1;
    while i >= 0 {
        if i < octaves && amplitudes[i as usize] != 0.0 {
            levels[i as usize] = Some(Perlin::new(random));
        } else {
            random.consume_count(262);
        }
        i -= 1;
    }
    assert!(zero_index >= octaves - 1, "positive octaves are not supported by the legacy nether noise");
    let mut factor = 2.0f64.powi(-zero_index);
    let mut value_factor = 2.0f64.powi(octaves - 1) / (2.0f64.powi(octaves) - 1.0);
    let mut stack = NoiseStack::new();
    for (i, level) in levels.into_iter().enumerate() {
        if let Some(noise) = level {
            stack.add(Octave::Perlin(noise), factor, (value_factor * amplitudes[i]) as f32);
        }
        factor *= 2.0;
        value_factor /= 2.0;
    }
    stack
}

/// The three fractal stacks of the legacy "old blended" terrain noise.
#[derive(Clone, Debug)]
pub struct BlendedStacks {
    pub min_limit: NoiseStack,
    pub max_limit: NoiseStack,
    pub main: NoiseStack,
}

const LIMIT_FACTOR: f64 = 0.999_984_741_210_937_5;
const MAIN_FACTOR: f64 = 12.75;

/// Smear scale of the limit stacks, `y_multiplier * smear_scale_multiplier`.
pub fn blended_fbm_set<R: RandomSource>(random: &mut R, y_scale: f64, smear_scale_multiplier: f64, y_factor: f64) -> BlendedStacks {
    let y_multiplier = 684.412 * y_scale;
    let limit_smear = y_multiplier * smear_scale_multiplier;
    let main_smear = limit_smear / y_factor;
    BlendedStacks {
        min_limit: create_fbm(random, -15, limit_smear, LIMIT_FACTOR),
        max_limit: create_fbm(random, -15, limit_smear, LIMIT_FACTOR),
        main: create_fbm(random, -7, main_smear, MAIN_FACTOR),
    }
}

fn create_fbm<R: RandomSource>(random: &mut R, first_octave: i32, smear_scale_y: f64, mut value_factor: f64) -> NoiseStack {
    assert!(first_octave <= 0);
    let octaves = -first_octave + 1;
    let mut factor = 1.0f64;
    value_factor /= 2.0f64.powi(octaves) - 1.0;
    let mut stack = NoiseStack::new();
    for _ in (0..octaves).rev() {
        stack.add(Octave::Smeared(Perlin::new(random), smear_scale_y * factor), factor, value_factor as f32);
        factor /= 2.0;
        value_factor *= 2.0;
    }
    stack
}

/// Value range of the blended noise as a function of its parameters.
pub fn blended_range(y_scale: f64, smear_scale_multiplier: f64) -> super::interval::Interval {
    use super::interval::Interval;
    let smear_scale_y = 684.412 * y_scale * smear_scale_multiplier;
    let octaves = 16;
    let mut factor = 1.0f64;
    let mut value_factor = LIMIT_FACTOR / (2.0f64.powi(octaves) - 1.0);
    let mut range = Interval::exact(0.0);
    for _ in 0..octaves {
        let layer = Interval::symmetric(((smear_scale_y * factor).abs() + 2.0) as f32);
        range = Interval::add(range, Interval::mul(layer, Interval::exact(value_factor as f32)));
        factor /= 2.0;
        value_factor *= 2.0;
    }
    range
}

/// Two-dimensional simplex noise, used only for the End's outer islands.
#[derive(Clone, Debug)]
pub struct Simplex {
    perms: [u8; 256],
}

impl Simplex {
    /// Seeds like a Perlin octave with the offsets discarded (still drawn).
    pub fn new_discarding_offset<R: RandomSource>(random: &mut R) -> Self {
        let p = Perlin::with_offset_scale(random, 0.0);
        Self { perms: p.perms }
    }

    fn permute(&self, x: i32) -> i32 {
        i32::from(self.perms[(x & 0xFF) as usize])
    }

    fn corner(index: i32, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let mut t0 = base - x * x - y * y - z * z;
        if t0 < 0.0 {
            0.0
        } else {
            t0 *= t0;
            let g = GRADIENT[index as usize];
            t0 * t0 * (f64::from(g[0]) * x + f64::from(g[1]) * y + f64::from(g[2]) * z)
        }
    }

    pub fn get(&self, xin: f64, yin: f64) -> f32 {
        // The discarded offsets are exactly zero.
        let sqrt3 = 3.0f64.sqrt();
        let f2 = 0.5 * (sqrt3 - 1.0);
        let g2 = (3.0 - sqrt3) / 6.0;
        let s = (xin + yin) * f2;
        let i = floor_i32(xin + s);
        let j = floor_i32(yin + s);
        let t = f64::from(i.wrapping_add(j)) * g2;
        let x0 = xin - (f64::from(i) - t);
        let y0 = yin - (f64::from(j) - t);
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - f64::from(i1) + g2;
        let y1 = y0 - f64::from(j1) + g2;
        let x2 = x0 - 1.0 + 2.0 * g2;
        let y2 = y0 - 1.0 + 2.0 * g2;
        let ii = i & 0xFF;
        let jj = j & 0xFF;
        let gi0 = self.permute(ii + self.permute(jj)) % 12;
        let gi1 = self.permute(ii + i1 + self.permute(jj + j1)) % 12;
        let gi2 = self.permute(ii + 1 + self.permute(jj + 1)) % 12;
        let n0 = Self::corner(gi0, x0, y0, 0.0, 0.5);
        let n1 = Self::corner(gi1, x1, y1, 0.0, 0.5);
        let n2 = Self::corner(gi2, x2, y2, 0.0, 0.5);
        (70.0 * (n0 + n1 + n2)) as f32
    }
}
