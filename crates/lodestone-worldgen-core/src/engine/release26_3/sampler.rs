//! The compiled sampler graph and its two evaluation paths.
//!
//! Every node can be asked for one value at a block position or for a whole
//! [`Volume`] at once. The two paths round differently (see `noise.rs`), so the
//! reference's answer to "what is the density here" depends on which path
//! served the request. A [`Ctx`] therefore carries the same cache-cell state
//! machine the reference keeps per chunk: a cell remembers the last volume it
//! was asked for and the last single value, and answers later requests from
//! them. Reordering sampling calls is not free for that reason.

use std::collections::HashMap;

use super::interval::{jmax, jmin};
use super::noise::{NoiseStack, Simplex};
use super::tree::{
    java_round, leaky_relu, round_to_integer, squeeze, Axis, Context as ContextKind, Metric, RoundKind, Tiling,
};
use super::volume::Volume;

pub type SId = u32;

#[derive(Clone, Debug)]
pub enum SplineS {
    Const(f32),
    Multi { coordinate: usize, locations: Vec<f32>, values: Vec<SplineS>, derivatives: Vec<f32> },
}

#[derive(Clone, Copy, Debug)]
pub enum GradientMode {
    Clamped { min: i32, max: i32 },
    Repeat { range: i32 },
    Mirrored { range: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Abs,
    Square,
    Cube,
    Sqrt,
    Reciprocal,
    Negate,
    Squeeze,
    Log,
    Sign,
}

#[derive(Clone, Debug)]
pub enum S {
    Const(f32),
    Context(ContextKind),
    Noise { noise: usize, xz: f64, y: f64 },
    ShiftedXz { shift_x: SId, shift_z: SId, noise: usize, xz: f64, y: f64 },
    ShiftedXyz { shift_x: SId, shift_y: SId, shift_z: SId, noise: usize, xz: f64, y: f64 },
    ShiftB { noise: usize },
    EndIslands { simplex: usize },
    Distance { point: [i32; 3], metric: Metric },
    Gradient { axis: Axis, from: i32, from_value: f32, factor: f32, mode: GradientMode },
    Unary(UnaryOp, SId),
    Leaky(SId, f32),
    Add(SId, SId),
    ConstAdd(SId, f32),
    Sub(SId, SId),
    ConstSub(f32, SId),
    Mul(SId, SId),
    ConstMul(SId, f32),
    Div(SId, SId),
    ConstDiv(f32, SId),
    Min { left: SId, right: SId, right_min: f32 },
    ConstMin(SId, f32),
    Max { left: SId, right: SId, right_max: f32 },
    ConstMax(SId, f32),
    PowConstBase(f64, SId),
    PowConstExponent(SId, f64),
    Pow(SId, SId),
    Round { kind: RoundKind, input: SId, multiple: Option<SId> },
    Spline { spline: SplineS, coordinates: Vec<SId> },
    LerpConstFirst { alpha: SId, first: f32, second: SId },
    LerpConstSecond { alpha: SId, first: SId, second: f32 },
    Lerp { alpha: SId, first: SId, second: SId },
    Clamp { input: SId, min: f32, max: f32 },
    RangeChoiceConst { input: SId, min: f32, max: f32, in_range: f32, out_of_range: f32 },
    RangeChoice { input: SId, min: f32, max: f32, in_range: SId, out_of_range: SId },
    SelectSingle { input: SId, threshold: f32, below: SId, above: SId },
    SelectMulti { input: SId, thresholds: Vec<f32>, samplers: Vec<SId> },
    BlendDensity(SId),
    Interpolated { input: SId, cell_xz: i32, cell_y: i32, inv_xz: f32, inv_y: f32 },
    Slice { axis: Axis, coordinate: i32, input: SId },
    SliceXz { x: i32, z: i32, input: SId },
    FindTopSurface { density: SId, upper_bound: SId, lower_bound: i32, cell_height: i32 },
    Cached { id: usize, input: SId },
}

/// The immutable, shareable compiled graph.
#[derive(Debug, Default)]
pub struct Program {
    pub samplers: Vec<S>,
    pub noises: Vec<NoiseStack>,
    pub simplexes: Vec<Simplex>,
    pub cache_count: usize,
}

/// One cache cell: the reference's per-chunk memo for a deduplicated cache.
#[derive(Default)]
struct Cell {
    volume: Option<Volume>,
    buffer: Option<Vec<f32>>,
    key: i64,
    value: f32,
}

/// Per-chunk mutable sampling state: cache cells, a buffer pool, context hooks.
pub struct Ctx {
    cells: Vec<Cell>,
    pool: Vec<Vec<f32>>,
    end_island_memo: HashMap<(i32, i32), f32>,
    /// Optional structure-density source for the `beardifier` leaf.
    pub beardifier: Option<Box<dyn BeardifierSource>>,
}

/// A source of structure-shaping density, sampled where a graph asks for the beardifier.
pub trait BeardifierSource: Send {
    fn value(&self, x: i32, y: i32, z: i32) -> f32;

    /// Fills `out` (laid out as `v`) with the source's value at every lattice point.
    fn fill_volume(&self, out: &mut [f32], v: &Volume) {
        let mut i = 0;
        for iz in 0..v.size[2] {
            for ix in 0..v.size[0] {
                for iy in 0..v.size[1] {
                    out[i] = self.value(v.block_x(ix), v.block_y(iy), v.block_z(iz));
                    i += 1;
                }
            }
        }
    }
}

impl Ctx {
    pub fn new(program: &Program) -> Self {
        Self::with_caches(program.cache_count)
    }

    /// A context with no caches: every request is computed afresh.
    pub fn uncached() -> Self {
        Self { cells: Vec::new(), pool: Vec::new(), end_island_memo: HashMap::new(), beardifier: None }
    }

    fn with_caches(n: usize) -> Self {
        let mut cells = Vec::with_capacity(n);
        cells.resize_with(n, || Cell { value: f32::NAN, ..Cell::default() });
        Self { cells, pool: Vec::new(), end_island_memo: HashMap::new(), beardifier: None }
    }

    fn take(&mut self, len: usize) -> Vec<f32> {
        // Contents are unspecified: every `volume` call overwrites its whole output
        // slice, so zeroing here would be pure memory traffic.
        let fit = self.pool.iter().rposition(|b| b.capacity() >= len);
        let mut v = match fit {
            Some(i) => self.pool.swap_remove(i),
            None => self.pool.pop().unwrap_or_default(),
        };
        if v.len() > len {
            v.truncate(len);
        } else {
            v.resize(len, 0.0);
        }
        v
    }

    fn give(&mut self, v: Vec<f32>) {
        self.pool.push(v);
    }

    /// Returns this context to its freshly constructed state while keeping its
    /// allocations, so one context can serve a sequence of chunks.
    pub fn reset(&mut self) {
        for cell in &mut self.cells {
            if let Some(b) = cell.buffer.take() {
                self.pool.push(b);
            }
            cell.volume = None;
            cell.key = 0;
            cell.value = f32::NAN;
        }
        self.beardifier = None;
        // Island heights depend only on coordinates, so the memo stays valid across chunks.
        if self.end_island_memo.len() > 1 << 14 {
            self.end_island_memo.clear();
        }
    }

    pub fn caches_enabled(&self) -> bool {
        !self.cells.is_empty()
    }
}

fn floor_mod(a: i32, b: i32) -> i32 {
    a.wrapping_sub(floor_div(a, b).wrapping_mul(b))
}

fn floor_div(a: i32, b: i32) -> i32 {
    let q = a.wrapping_div(b);
    if (a ^ b) < 0 && q.wrapping_mul(b) != a { q - 1 } else { q }
}

/// Packs a block position into one key: 26 bits of X, 26 of Z, 12 of Y.
pub fn pack_block(x: i32, y: i32, z: i32) -> i64 {
    ((i64::from(x) & 0x3FF_FFFF) << 38) | ((i64::from(z) & 0x3FF_FFFF) << 12) | (i64::from(y) & 0xFFF)
}

fn lerp(alpha: f32, p0: f32, p1: f32) -> f32 {
    p0 + alpha * (p1 - p0)
}

#[allow(clippy::too_many_arguments)]
fn lerp3(a1: f32, a2: f32, a3: f32, x000: f32, x100: f32, x010: f32, x110: f32, x001: f32, x101: f32, x011: f32, x111: f32) -> f32 {
    let l2 = |x00, x10, x01, x11| lerp(a2, lerp(a1, x00, x10), lerp(a1, x01, x11));
    lerp(a3, l2(x000, x100, x010, x110), l2(x001, x101, x011, x111))
}

fn clamp_f(value: f32, min: f32, max: f32) -> f32 {
    if value < min { min } else { jmin(value, max) }
}

fn metric3(metric: Metric, dx: f32, dy: f32, dz: f32) -> f32 {
    match metric {
        Metric::Euclidean => (dx * dx + dy * dy + dz * dz).sqrt(),
        Metric::EuclideanSquared => dx * dx + dy * dy + dz * dz,
        Metric::Manhattan => dx.abs() + dy.abs() + dz.abs(),
        Metric::Chebyshev => jmax(jmax(dx.abs(), dy.abs()), dz.abs()),
    }
}

fn gradient_value(mode: GradientMode, from: i32, from_value: f32, factor: f32, coordinate: i32) -> f32 {
    match mode {
        GradientMode::Clamped { min, max } => {
            let rel = coordinate.max(min).min(max).wrapping_sub(from);
            from_value + rel as f32 * factor
        }
        GradientMode::Repeat { range } => {
            let rel = coordinate.wrapping_sub(from);
            from_value + floor_mod(rel, range) as f32 * factor
        }
        GradientMode::Mirrored { range } => {
            let rel = coordinate.wrapping_sub(from);
            let tile = floor_div(rel, range);
            let local = rel.wrapping_sub(tile.wrapping_mul(range));
            if tile & 1 == 0 {
                from_value + local as f32 * factor
            } else {
                from_value + range.wrapping_sub(local) as f32 * factor
            }
        }
    }
}

fn spline_mix<F: FnMut(usize) -> f32>(spline: &SplineS, coord: &mut F) -> f32 {
    match spline {
        SplineS::Const(v) => *v,
        SplineS::Multi { coordinate, locations, values, derivatives } => {
            let input = coord(*coordinate);
            // First index whose location exceeds the input, minus one.
            let (mut from, mut len) = (0usize, locations.len());
            while len > 0 {
                let half = len / 2;
                let middle = from + half;
                if input < locations[middle] {
                    len = half;
                } else {
                    from = middle + 1;
                    len -= half + 1;
                }
            }
            let start = from as isize - 1;
            let last = locations.len() - 1;
            let extend = |value: f32, index: usize| {
                let d = derivatives[index];
                if d == 0.0 { value } else { value + d * (input - locations[index]) }
            };
            if start < 0 {
                let v = spline_mix(&values[0], coord);
                return extend(v, 0);
            }
            let start = start as usize;
            if start == last {
                let v = spline_mix(&values[last], coord);
                return extend(v, last);
            }
            let (x1, x2) = (locations[start], locations[start + 1]);
            let t = (input - x1) / (x2 - x1);
            let (d1, d2) = (derivatives[start], derivatives[start + 1]);
            let y1 = spline_mix(&values[start], coord);
            let y2 = spline_mix(&values[start + 1], coord);
            let a = d1 * (x2 - x1) - (y2 - y1);
            let b = -d2 * (x2 - x1) + (y2 - y1);
            lerp(t, y1, y2) + t * (1.0 - t) * lerp(t, a, b)
        }
    }
}

fn round_apply(kind: RoundKind, input: f32, multiple: f32) -> f32 {
    if multiple == 0.0 { input } else { round_to_integer(input / multiple, kind) * multiple }
}

fn pow_f(base: f64, exp: f64) -> f32 {
    java_pow(base, exp) as f32
}

/// `Math.pow` for the cases the reference's intrinsic treats specially.
fn java_pow(base: f64, exp: f64) -> f64 {
    if exp == 0.0 {
        return 1.0;
    }
    if exp.is_nan() || base.is_nan() {
        return f64::NAN;
    }
    if base.abs() == 1.0 && exp.is_infinite() {
        return f64::NAN;
    }
    base.powf(exp)
}

impl Program {
    // ---------------------------------------------------------------- scalar

    /// The value at one block position.
    pub fn value(&self, ctx: &mut Ctx, id: SId, x: i32, y: i32, z: i32) -> f32 {
        match &self.samplers[id as usize] {
            S::Const(v) => *v,
            S::Context(kind) => match kind {
                ContextKind::BlendAlpha => 1.0,
                ContextKind::BlendOffset => 0.0,
                ContextKind::Beardifier => ctx.beardifier.as_ref().map_or(0.0, |b| b.value(x, y, z)),
            },
            S::Noise { noise, xz, y: ys } => {
                self.noises[*noise].get(f64::from(x) * xz, f64::from(y) * ys, f64::from(z) * xz)
            }
            S::ShiftedXz { shift_x, shift_z, noise, xz, y: ys } => {
                let nx = f64::from(x) * xz + f64::from(self.value(ctx, *shift_x, x, y, z));
                let ny = f64::from(y) * ys;
                let nz = f64::from(z) * xz + f64::from(self.value(ctx, *shift_z, x, y, z));
                self.noises[*noise].get(nx, ny, nz)
            }
            S::ShiftedXyz { shift_x, shift_y, shift_z, noise, xz, y: ys } => {
                let nx = f64::from(x) * xz + f64::from(self.value(ctx, *shift_x, x, y, z));
                let ny = f64::from(y) * ys + f64::from(self.value(ctx, *shift_y, x, y, z));
                let nz = f64::from(z) * xz + f64::from(self.value(ctx, *shift_z, x, y, z));
                self.noises[*noise].get(nx, ny, nz)
            }
            S::ShiftB { noise } => self.noises[*noise].get(f64::from(z) * 0.25, f64::from(x) * 0.25, 0.0) * 4.0,
            S::EndIslands { simplex } => self.end_island(ctx, *simplex, x, z),
            S::Distance { point, metric } => {
                metric3(*metric, point[0].wrapping_sub(x) as f32, point[1].wrapping_sub(y) as f32, point[2].wrapping_sub(z) as f32)
            }
            S::Gradient { axis, from, from_value, factor, mode } => {
                let c = match axis {
                    Axis::X => x,
                    Axis::Y => y,
                    Axis::Z => z,
                };
                gradient_value(*mode, *from, *from_value, *factor, c)
            }
            S::Unary(op, input) => {
                let v = self.value(ctx, *input, x, y, z);
                unary_apply(*op, v)
            }
            S::Leaky(input, f) => leaky_relu(*f, self.value(ctx, *input, x, y, z)),
            S::Add(l, r) => {
                let a = self.value(ctx, *l, x, y, z);
                a + self.value(ctx, *r, x, y, z)
            }
            S::ConstAdd(l, c) => self.value(ctx, *l, x, y, z) + c,
            S::Sub(l, r) => {
                let a = self.value(ctx, *l, x, y, z);
                a - self.value(ctx, *r, x, y, z)
            }
            S::ConstSub(c, r) => c - self.value(ctx, *r, x, y, z),
            S::Mul(l, r) => {
                let a = self.value(ctx, *l, x, y, z);
                if a == 0.0 { 0.0 } else { a * self.value(ctx, *r, x, y, z) }
            }
            S::ConstMul(l, c) => self.value(ctx, *l, x, y, z) * c,
            S::Div(l, r) => {
                let a = self.value(ctx, *l, x, y, z);
                if a == 0.0 { 0.0 } else { a / self.value(ctx, *r, x, y, z) }
            }
            S::ConstDiv(c, r) => c / self.value(ctx, *r, x, y, z),
            S::Min { left, right, right_min } => {
                let a = self.value(ctx, *left, x, y, z);
                if a <= *right_min { a } else { jmin(a, self.value(ctx, *right, x, y, z)) }
            }
            S::ConstMin(l, c) => jmin(self.value(ctx, *l, x, y, z), *c),
            S::Max { left, right, right_max } => {
                let a = self.value(ctx, *left, x, y, z);
                if a >= *right_max { a } else { jmax(a, self.value(ctx, *right, x, y, z)) }
            }
            S::ConstMax(l, c) => jmax(self.value(ctx, *l, x, y, z), *c),
            S::PowConstBase(b, e) => pow_f(*b, f64::from(self.value(ctx, *e, x, y, z))),
            S::PowConstExponent(b, e) => pow_f(f64::from(self.value(ctx, *b, x, y, z)), *e),
            S::Pow(b, e) => {
                let base = self.value(ctx, *b, x, y, z);
                pow_f(f64::from(base), f64::from(self.value(ctx, *e, x, y, z)))
            }
            S::Round { kind, input, multiple } => {
                let v = self.value(ctx, *input, x, y, z);
                match multiple {
                    None => round_to_integer(v, *kind),
                    Some(m) => {
                        let m = self.value(ctx, *m, x, y, z);
                        round_apply(*kind, v, m)
                    }
                }
            }
            S::Spline { spline, coordinates } => {
                let mut cached = vec![f32::NAN; coordinates.len()];
                spline_mix(spline, &mut |i| {
                    let c = cached[i];
                    if !c.is_nan() {
                        return c;
                    }
                    let v = self.value(ctx, coordinates[i], x, y, z);
                    cached[i] = v;
                    v
                })
            }
            S::LerpConstFirst { alpha, first, second } => {
                let a = self.value(ctx, *alpha, x, y, z);
                if a == 0.0 {
                    *first
                } else if a == 1.0 {
                    self.value(ctx, *second, x, y, z)
                } else {
                    lerp(a, *first, self.value(ctx, *second, x, y, z))
                }
            }
            S::LerpConstSecond { alpha, first, second } => {
                let a = self.value(ctx, *alpha, x, y, z);
                if a == 0.0 {
                    self.value(ctx, *first, x, y, z)
                } else if a == 1.0 {
                    *second
                } else {
                    lerp(a, self.value(ctx, *first, x, y, z), *second)
                }
            }
            S::Lerp { alpha, first, second } => {
                let a = self.value(ctx, *alpha, x, y, z);
                if a == 0.0 {
                    self.value(ctx, *first, x, y, z)
                } else if a == 1.0 {
                    self.value(ctx, *second, x, y, z)
                } else {
                    let f = self.value(ctx, *first, x, y, z);
                    lerp(a, f, self.value(ctx, *second, x, y, z))
                }
            }
            S::Clamp { input, min, max } => clamp_f(self.value(ctx, *input, x, y, z), *min, *max),
            S::RangeChoiceConst { input, min, max, in_range, out_of_range } => {
                let v = self.value(ctx, *input, x, y, z);
                if v >= *min && v < *max { *in_range } else { *out_of_range }
            }
            S::RangeChoice { input, min, max, in_range, out_of_range } => {
                let v = self.value(ctx, *input, x, y, z);
                if v >= *min && v < *max {
                    self.value(ctx, *in_range, x, y, z)
                } else {
                    self.value(ctx, *out_of_range, x, y, z)
                }
            }
            S::SelectSingle { input, threshold, below, above } => {
                let v = self.value(ctx, *input, x, y, z);
                if v < *threshold { self.value(ctx, *below, x, y, z) } else { self.value(ctx, *above, x, y, z) }
            }
            S::SelectMulti { input, thresholds, samplers } => {
                let v = self.value(ctx, *input, x, y, z);
                let i = select_index(thresholds, samplers.len(), v);
                self.value(ctx, samplers[i], x, y, z)
            }
            S::BlendDensity(input) => self.value(ctx, *input, x, y, z),
            S::Interpolated { input, cell_xz, cell_y, inv_xz: _, inv_y: _ } => {
                let xin = floor_mod(x, *cell_xz);
                let yin = floor_mod(y, *cell_y);
                let zin = floor_mod(z, *cell_xz);
                if xin == 0 && yin == 0 && zin == 0 {
                    return self.value(ctx, *input, x, y, z);
                }
                let volume = Volume::new([2, 2, 2], [x - xin, y - yin, z - zin], [*cell_xz, *cell_y, *cell_xz]);
                let mut buf = ctx.take(8);
                self.volume(ctx, *input, &mut buf, &volume);
                let r = lerp3(
                    xin as f32 / *cell_xz as f32,
                    yin as f32 / *cell_y as f32,
                    zin as f32 / *cell_xz as f32,
                    buf[volume.index(0, 0, 0)],
                    buf[volume.index(1, 0, 0)],
                    buf[volume.index(0, 1, 0)],
                    buf[volume.index(1, 1, 0)],
                    buf[volume.index(0, 0, 1)],
                    buf[volume.index(1, 0, 1)],
                    buf[volume.index(0, 1, 1)],
                    buf[volume.index(1, 1, 1)],
                );
                ctx.give(buf);
                r
            }
            S::Slice { axis, coordinate, input } => match axis {
                Axis::X => self.value(ctx, *input, *coordinate, y, z),
                Axis::Y => self.value(ctx, *input, x, *coordinate, z),
                Axis::Z => self.value(ctx, *input, x, y, *coordinate),
            },
            S::SliceXz { x: sx, z: sz, input } => self.value(ctx, *input, *sx, y, *sz),
            S::FindTopSurface { density, upper_bound, lower_bound, cell_height } => {
                let upper = self.value(ctx, *upper_bound, x, y, z);
                self.find_surface(ctx, *density, x, z, upper, *lower_bound, *cell_height) as f32
            }
            S::Cached { id, input } => self.value_cached(ctx, *id, *input, x, y, z),
        }
    }

    fn value_cached(&self, ctx: &mut Ctx, id: usize, input: SId, x: i32, y: i32, z: i32) -> f32 {
        if !ctx.caches_enabled() {
            return self.value(ctx, input, x, y, z);
        }
        let key = pack_block(x, y, z);
        {
            let cell = &ctx.cells[id];
            if cell.key == key && !cell.value.is_nan() {
                return cell.value;
            }
            if let (Some(buffer), Some(volume)) = (&cell.buffer, &cell.volume) {
                if let Some(index) = volume.index_of_block(x, y, z) {
                    return buffer[index];
                }
            }
        }
        let value = self.value(ctx, input, x, y, z);
        let cell = &mut ctx.cells[id];
        cell.key = key;
        cell.value = value;
        value
    }

    #[allow(clippy::too_many_arguments)]
    fn find_surface(&self, ctx: &mut Ctx, density: SId, x: i32, z: i32, upper: f32, lower: i32, cell_height: i32) -> i32 {
        let top = (f64::from(upper / cell_height as f32).floor() as i32).wrapping_mul(cell_height);
        if top <= lower {
            return lower;
        }
        let mut probe = top;
        while probe >= lower {
            if self.value(ctx, density, x, probe, z) > 0.0 {
                return probe;
            }
            probe -= cell_height;
        }
        lower
    }

    fn end_island(&self, ctx: &mut Ctx, simplex: usize, x: i32, z: i32) -> f32 {
        let (sx, sz) = (x / 8, z / 8);
        let height = if let Some(v) = ctx.end_island_memo.get(&(sx, sz)) {
            *v
        } else {
            let v = end_height(&self.simplexes[simplex], sx, sz);
            ctx.end_island_memo.insert((sx, sz), v);
            v
        };
        (height - 8.0) / 128.0
    }

    // ---------------------------------------------------------------- volume

    /// Fills `out` (length `volume.len()`) with the graph's values over a volume.
    pub fn volume(&self, ctx: &mut Ctx, id: SId, out: &mut [f32], v: &Volume) {
        debug_assert_eq!(out.len(), v.len());
        match &self.samplers[id as usize] {
            S::Const(c) => out.fill(*c),
            S::Context(kind) => match kind {
                ContextKind::BlendAlpha => out.fill(1.0),
                ContextKind::BlendOffset => out.fill(0.0),
                ContextKind::Beardifier => {
                    match ctx.beardifier.as_ref() {
                        Some(b) => b.fill_volume(out, v),
                        None => out.fill(0.0),
                    }
                }
            },
            S::Noise { noise, xz, y } => {
                out.fill(0.0);
                self.noises[*noise].add_to_volume(out, v, *xz, *y, 1.0);
            }
            S::ShiftedXz { shift_x, shift_z, noise, xz, y } => {
                self.volume(ctx, *shift_x, out, v);
                let mut sz = ctx.take(out.len());
                self.volume(ctx, *shift_z, &mut sz, v);
                let mut i = 0;
                for iz in 0..v.size[2] {
                    let base_z = f64::from(v.block_z(iz)) * xz;
                    for ix in 0..v.size[0] {
                        let base_x = f64::from(v.block_x(ix)) * xz;
                        for iy in 0..v.size[1] {
                            let nx = base_x + f64::from(out[i]);
                            let ny = f64::from(v.block_y(iy)) * y;
                            let nz = base_z + f64::from(sz[i]);
                            out[i] = self.noises[*noise].get(nx, ny, nz);
                            i += 1;
                        }
                    }
                }
                ctx.give(sz);
            }
            S::ShiftedXyz { shift_x, shift_y, shift_z, noise, xz, y } => {
                self.volume(ctx, *shift_x, out, v);
                let mut sy = ctx.take(out.len());
                self.volume(ctx, *shift_y, &mut sy, v);
                let mut sz = ctx.take(out.len());
                self.volume(ctx, *shift_z, &mut sz, v);
                let mut i = 0;
                for iz in 0..v.size[2] {
                    let base_z = f64::from(v.block_z(iz)) * xz;
                    for ix in 0..v.size[0] {
                        let base_x = f64::from(v.block_x(ix)) * xz;
                        for iy in 0..v.size[1] {
                            let nx = base_x + f64::from(out[i]);
                            let ny = f64::from(v.block_y(iy)) * y + f64::from(sy[i]);
                            let nz = base_z + f64::from(sz[i]);
                            out[i] = self.noises[*noise].get(nx, ny, nz);
                            i += 1;
                        }
                    }
                }
                ctx.give(sz);
                ctx.give(sy);
            }
            S::ShiftB { noise } => {
                let t = Volume::new(
                    [v.size[2], v.size[0], 1],
                    [v.min[2], v.min[0], 0],
                    [v.step[2], v.step[0], 1],
                );
                let mut tb = ctx.take(t.len());
                tb.fill(0.0);
                self.noises[*noise].add_to_volume(&mut tb, &t, 0.25, 0.25, 4.0);
                for iz in 0..v.size[2] {
                    for ix in 0..v.size[0] {
                        let value = tb[t.index(iz, ix, 0)];
                        let start = v.index(ix, 0, iz);
                        out[start..start + v.size[1] as usize].fill(value);
                    }
                }
                ctx.give(tb);
            }
            S::EndIslands { simplex } => {
                for iz in 0..v.size[2] {
                    let bz = v.block_z(iz);
                    for ix in 0..v.size[0] {
                        let value = self.end_island(ctx, *simplex, v.block_x(ix), bz);
                        let start = v.index(ix, 0, iz);
                        out[start..start + v.size[1] as usize].fill(value);
                    }
                }
            }
            S::Distance { point, metric } => {
                let mut i = 0;
                for iz in 0..v.size[2] {
                    let bz = v.block_z(iz);
                    for ix in 0..v.size[0] {
                        let bx = v.block_x(ix);
                        for iy in 0..v.size[1] {
                            let by = v.block_y(iy);
                            out[i] = metric3(
                                *metric,
                                point[0].wrapping_sub(bx) as f32,
                                point[1].wrapping_sub(by) as f32,
                                point[2].wrapping_sub(bz) as f32,
                            );
                            i += 1;
                        }
                    }
                }
            }
            S::Gradient { axis, from, from_value, factor, mode } => {
                let g = |c: i32| gradient_value(*mode, *from, *from_value, *factor, c);
                match axis {
                    Axis::X => {
                        for ix in 0..v.size[0] {
                            let value = g(v.block_x(ix));
                            for iz in 0..v.size[2] {
                                let start = v.index(ix, 0, iz);
                                out[start..start + v.size[1] as usize].fill(value);
                            }
                        }
                    }
                    Axis::Y => {
                        for iy in 0..v.size[1] {
                            let value = g(v.block_y(iy));
                            for iz in 0..v.size[2] {
                                for ix in 0..v.size[0] {
                                    out[v.index(ix, iy, iz)] = value;
                                }
                            }
                        }
                    }
                    Axis::Z => {
                        let plane = (v.size[0] * v.size[1]) as usize;
                        for iz in 0..v.size[2] {
                            let value = g(v.block_z(iz));
                            let start = v.index(0, 0, iz);
                            out[start..start + plane].fill(value);
                        }
                    }
                }
            }
            S::Unary(op, input) => {
                self.volume(ctx, *input, out, v);
                for o in out.iter_mut() {
                    *o = unary_apply(*op, *o);
                }
            }
            S::Leaky(input, f) => {
                self.volume(ctx, *input, out, v);
                for o in out.iter_mut() {
                    *o = leaky_relu(*f, *o);
                }
            }
            S::Add(l, r) => {
                self.volume(ctx, *l, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *r, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    *o += *r;
                }
                ctx.give(rb);
            }
            S::ConstAdd(l, c) => {
                self.volume(ctx, *l, out, v);
                for o in out.iter_mut() {
                    *o += *c;
                }
            }
            S::Sub(l, r) => {
                self.volume(ctx, *l, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *r, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    *o += -*r;
                }
                ctx.give(rb);
            }
            S::ConstSub(c, r) => {
                self.volume(ctx, *r, out, v);
                for o in out.iter_mut() {
                    *o = *c - *o;
                }
            }
            S::Mul(l, r) => {
                self.volume(ctx, *l, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *r, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    *o *= *r;
                }
                ctx.give(rb);
            }
            S::ConstMul(l, c) => {
                self.volume(ctx, *l, out, v);
                for o in out.iter_mut() {
                    *o *= *c;
                }
            }
            S::Div(l, r) => {
                self.volume(ctx, *l, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *r, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    *o /= *r;
                }
                ctx.give(rb);
            }
            S::ConstDiv(c, r) => {
                self.volume(ctx, *r, out, v);
                for o in out.iter_mut() {
                    *o = *c / *o;
                }
            }
            S::Min { left, right, .. } => {
                self.volume(ctx, *left, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *right, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    if *r < *o {
                        *o = *r;
                    }
                }
                ctx.give(rb);
            }
            S::ConstMin(l, c) => {
                self.volume(ctx, *l, out, v);
                for o in out.iter_mut() {
                    if *c < *o {
                        *o = *c;
                    }
                }
            }
            S::Max { left, right, .. } => {
                self.volume(ctx, *left, out, v);
                let mut rb = ctx.take(out.len());
                self.volume(ctx, *right, &mut rb, v);
                for (o, r) in out.iter_mut().zip(&rb) {
                    if *r > *o {
                        *o = *r;
                    }
                }
                ctx.give(rb);
            }
            S::ConstMax(l, c) => {
                self.volume(ctx, *l, out, v);
                for o in out.iter_mut() {
                    if *c > *o {
                        *o = *c;
                    }
                }
            }
            S::PowConstBase(b, e) => {
                self.volume(ctx, *e, out, v);
                for o in out.iter_mut() {
                    *o = pow_f(*b, f64::from(*o));
                }
            }
            S::PowConstExponent(b, e) => {
                self.volume(ctx, *b, out, v);
                for o in out.iter_mut() {
                    *o = pow_f(f64::from(*o), *e);
                }
            }
            S::Pow(b, e) => {
                self.volume(ctx, *b, out, v);
                let mut eb = ctx.take(out.len());
                self.volume(ctx, *e, &mut eb, v);
                for (o, e) in out.iter_mut().zip(&eb) {
                    *o = pow_f(f64::from(*o), f64::from(*e));
                }
                ctx.give(eb);
            }
            S::Round { kind, input, multiple } => {
                self.volume(ctx, *input, out, v);
                match multiple {
                    None => {
                        for o in out.iter_mut() {
                            *o = round_to_integer(*o, *kind);
                        }
                    }
                    Some(m) => {
                        let mut mb = ctx.take(out.len());
                        self.volume(ctx, *m, &mut mb, v);
                        for (o, m) in out.iter_mut().zip(&mb) {
                            *o = round_apply(*kind, *o, *m);
                        }
                        ctx.give(mb);
                    }
                }
            }
            S::Spline { spline, coordinates } => {
                let mut buffers: Vec<Option<Vec<f32>>> = (0..coordinates.len()).map(|_| None).collect();
                for (i, o) in out.iter_mut().enumerate() {
                    *o = spline_mix(spline, &mut |k| {
                        if buffers[k].is_none() {
                            let mut b = ctx.take(v.len());
                            self.volume(ctx, coordinates[k], &mut b, v);
                            buffers[k] = Some(b);
                        }
                        buffers[k].as_ref().expect("filled above")[i]
                    });
                }
                for b in buffers.into_iter().flatten() {
                    ctx.give(b);
                }
            }
            S::LerpConstFirst { alpha, first, second } => {
                self.volume(ctx, *alpha, out, v);
                let mut sb = ctx.take(out.len());
                self.volume(ctx, *second, &mut sb, v);
                for (o, s) in out.iter_mut().zip(&sb) {
                    let a = *o;
                    *o = if a == 0.0 { *first } else if a == 1.0 { *s } else { lerp(a, *first, *s) };
                }
                ctx.give(sb);
            }
            S::LerpConstSecond { alpha, first, second } => {
                self.volume(ctx, *alpha, out, v);
                let mut fb = ctx.take(out.len());
                self.volume(ctx, *first, &mut fb, v);
                for (o, f) in out.iter_mut().zip(&fb) {
                    let a = *o;
                    *o = if a == 0.0 { *f } else if a == 1.0 { *second } else { lerp(a, *f, *second) };
                }
                ctx.give(fb);
            }
            S::Lerp { alpha, first, second } => {
                self.volume(ctx, *alpha, out, v);
                let mut fb = ctx.take(out.len());
                self.volume(ctx, *first, &mut fb, v);
                let mut sb = ctx.take(out.len());
                self.volume(ctx, *second, &mut sb, v);
                for ((o, f), s) in out.iter_mut().zip(&fb).zip(&sb) {
                    let a = *o;
                    *o = if a == 0.0 { *f } else if a == 1.0 { *s } else { lerp(a, *f, *s) };
                }
                ctx.give(sb);
                ctx.give(fb);
            }
            S::Clamp { input, min, max } => {
                self.volume(ctx, *input, out, v);
                for o in out.iter_mut() {
                    *o = clamp_f(*o, *min, *max);
                }
            }
            S::RangeChoiceConst { input, min, max, in_range, out_of_range } => {
                self.volume(ctx, *input, out, v);
                for o in out.iter_mut() {
                    *o = if *o >= *min && *o < *max { *in_range } else { *out_of_range };
                }
            }
            S::RangeChoice { input, min, max, in_range, out_of_range } => {
                self.volume(ctx, *in_range, out, v);
                let mut ib = ctx.take(out.len());
                self.volume(ctx, *input, &mut ib, v);
                let mut ob = ctx.take(out.len());
                self.volume(ctx, *out_of_range, &mut ob, v);
                for ((o, i), alt) in out.iter_mut().zip(&ib).zip(&ob) {
                    if !(*i >= *min) || !(*i < *max) {
                        *o = *alt;
                    }
                }
                ctx.give(ob);
                ctx.give(ib);
            }
            S::SelectSingle { input, threshold, below, above } => {
                self.volume(ctx, *input, out, v);
                let mut bb = ctx.take(out.len());
                self.volume(ctx, *below, &mut bb, v);
                let mut ab = ctx.take(out.len());
                self.volume(ctx, *above, &mut ab, v);
                for ((o, b), a) in out.iter_mut().zip(&bb).zip(&ab) {
                    *o = if *o < *threshold { *b } else { *a };
                }
                ctx.give(ab);
                ctx.give(bb);
            }
            S::SelectMulti { input, thresholds, samplers } => {
                self.volume(ctx, *input, out, v);
                let mut buffers = Vec::with_capacity(samplers.len());
                for s in samplers {
                    let mut b = ctx.take(out.len());
                    self.volume(ctx, *s, &mut b, v);
                    buffers.push(b);
                }
                for (i, o) in out.iter_mut().enumerate() {
                    *o = buffers[select_index(thresholds, samplers.len(), *o)][i];
                }
                for b in buffers {
                    ctx.give(b);
                }
            }
            S::BlendDensity(input) => self.volume(ctx, *input, out, v),
            S::Interpolated { input, cell_xz, cell_y, inv_xz, inv_y } => {
                let on_lattice = (v.step[0] == *cell_xz || v.size[0] == 1)
                    && (v.step[1] == *cell_y || v.size[1] == 1)
                    && (v.step[2] == *cell_xz || v.size[2] == 1)
                    && floor_mod(v.min[0], *cell_xz) == 0
                    && floor_mod(v.min[1], *cell_y) == 0
                    && floor_mod(v.min[2], *cell_xz) == 0;
                if on_lattice {
                    self.volume(ctx, *input, out, v);
                } else if v.step == [1, 1, 1] {
                    self.interpolate_blocks(ctx, *input, out, v, *cell_xz, *cell_y, *inv_xz, *inv_y);
                } else {
                    let block = Volume::new(
                        [v.size[0] * v.step[0], v.size[1] * v.step[1], v.size[2] * v.step[2]],
                        v.min,
                        [1, 1, 1],
                    );
                    let mut bb = ctx.take(block.len());
                    self.interpolate_blocks(ctx, *input, &mut bb, &block, *cell_xz, *cell_y, *inv_xz, *inv_y);
                    for iz in 0..v.size[2] {
                        for ix in 0..v.size[0] {
                            for iy in 0..v.size[1] {
                                out[v.index(ix, iy, iz)] =
                                    bb[block.index(ix * v.step[0], iy * v.step[1], iz * v.step[2])];
                            }
                        }
                    }
                    ctx.give(bb);
                }
            }
            S::Slice { axis, coordinate, input } => self.slice_volume(ctx, *axis, *coordinate, *input, out, v),
            S::SliceXz { x, z, input } => {
                if v.size[0] == 1 && v.size[2] == 1 && v.min[0] == *x && v.min[2] == *z {
                    self.volume(ctx, *input, out, v);
                } else {
                    let iv = Volume::new([1, v.size[1], 1], [*x, v.min[1], *z], v.step);
                    let mut ib = ctx.take(iv.len());
                    self.volume(ctx, *input, &mut ib, &iv);
                    for iy in 0..v.size[1] {
                        let value = ib[iv.index(0, iy, 0)];
                        let mut index = v.index(0, iy, 0);
                        for _ in 0..v.size[2] {
                            for _ in 0..v.size[0] {
                                out[index] = value;
                                index += v.size[1] as usize;
                            }
                        }
                    }
                    ctx.give(ib);
                }
            }
            S::FindTopSurface { density, upper_bound, lower_bound, cell_height } => {
                assert_eq!(v.size[1], 1, "cannot sample find_top_surface with sizeY != 1");
                self.volume(ctx, *upper_bound, out, v);
                let mut i = 0;
                for iz in 0..v.size[2] {
                    let bz = v.block_z(iz);
                    for ix in 0..v.size[0] {
                        let bx = v.block_x(ix);
                        let upper = out[i];
                        out[i] = self.find_surface(ctx, *density, bx, bz, upper, *lower_bound, *cell_height) as f32;
                        i += 1;
                    }
                }
            }
            S::Cached { id, input } => self.volume_cached(ctx, *id, *input, out, v),
        }
    }

    fn fill_pointwise(&self, ctx: &mut Ctx, out: &mut [f32], v: &Volume, f: impl Fn(&mut Ctx, i32, i32, i32) -> f32) {
        let mut i = 0;
        for iz in 0..v.size[2] {
            for ix in 0..v.size[0] {
                for iy in 0..v.size[1] {
                    out[i] = f(ctx, v.block_x(ix), v.block_y(iy), v.block_z(iz));
                    i += 1;
                }
            }
        }
    }

    fn volume_cached(&self, ctx: &mut Ctx, id: usize, input: SId, out: &mut [f32], v: &Volume) {
        if !ctx.caches_enabled() {
            self.volume(ctx, input, out, v);
            return;
        }
        let stale = {
            let cell = &ctx.cells[id];
            cell.buffer.is_none() || cell.volume != Some(*v)
        };
        if stale {
            if let Some(old) = ctx.cells[id].buffer.take() {
                ctx.give(old);
            }
            ctx.cells[id].volume = Some(*v);
            let mut buffer = ctx.take(v.len());
            self.volume(ctx, input, &mut buffer, v);
            ctx.cells[id].buffer = Some(buffer);
        }
        out.copy_from_slice(ctx.cells[id].buffer.as_ref().expect("cell buffer present"));
    }

    fn slice_volume(&self, ctx: &mut Ctx, axis: Axis, coordinate: i32, input: SId, out: &mut [f32], v: &Volume) {
        let a = axis.index();
        if v.size[a] == 1 && v.min[a] == coordinate {
            self.volume(ctx, input, out, v);
            return;
        }
        let mut size = v.size;
        let mut min = v.min;
        size[a] = 1;
        min[a] = coordinate;
        let iv = Volume::new(size, min, v.step);
        let mut ib = ctx.take(iv.len());
        self.volume(ctx, input, &mut ib, &iv);
        match axis {
            Axis::X => {
                let mut index = 0;
                for iz in 0..v.size[2] {
                    for _ in 0..v.size[0] {
                        for iy in 0..v.size[1] {
                            out[index] = ib[iv.index(0, iy, iz)];
                            index += 1;
                        }
                    }
                }
            }
            Axis::Z => {
                let mut index = 0;
                for _ in 0..v.size[2] {
                    for ix in 0..v.size[0] {
                        for iy in 0..v.size[1] {
                            out[index] = ib[iv.index(ix, iy, 0)];
                            index += 1;
                        }
                    }
                }
            }
            Axis::Y => {
                for iz in 0..v.size[2] {
                    for ix in 0..v.size[0] {
                        let value = ib[iv.index(ix, 0, iz)];
                        let start = v.index(ix, 0, iz);
                        out[start..start + v.size[1] as usize].fill(value);
                    }
                }
            }
        }
        ctx.give(ib);
    }

    #[allow(clippy::too_many_arguments)]
    fn interpolate_blocks(&self, ctx: &mut Ctx, input: SId, out: &mut [f32], v: &Volume, cxz: i32, cy: i32, inv_xz: f32, inv_y: f32) {
        let min_cell = [floor_div(v.min[0], cxz), floor_div(v.min[1], cy), floor_div(v.min[2], cxz)];
        let max_block = [v.max_block(0), v.max_block(1), v.max_block(2)];
        let max_cell = [floor_div(max_block[0], cxz), floor_div(max_block[1], cy), floor_div(max_block[2], cxz)];
        let count = [max_cell[0] - min_cell[0] + 1, max_cell[1] - min_cell[1] + 1, max_cell[2] - min_cell[2] + 1];
        let cell_volume = Volume::new(
            [
                if floor_mod(max_block[0], cxz) == 0 { count[0] } else { count[0] + 1 },
                if floor_mod(max_block[1], cy) == 0 { count[1] } else { count[1] + 1 },
                if floor_mod(max_block[2], cxz) == 0 { count[2] } else { count[2] + 1 },
            ],
            [min_cell[0] * cxz, min_cell[1] * cy, min_cell[2] * cxz],
            [cxz, cy, cxz],
        );
        let mut cb = ctx.take(cell_volume.len());
        self.volume(ctx, input, &mut cb, &cell_volume);
        for cz in 0..count[2] {
            let next_z = (cz + 1).min(cell_volume.size[2] - 1);
            for cx in 0..count[0] {
                let next_x = (cx + 1).min(cell_volume.size[0] - 1);
                let mut v000 = cb[cell_volume.index(cx, 0, cz)];
                let mut v100 = cb[cell_volume.index(next_x, 0, cz)];
                let mut v001 = cb[cell_volume.index(cx, 0, next_z)];
                let mut v101 = cb[cell_volume.index(next_x, 0, next_z)];
                for cy_i in 0..count[1] {
                    let next_y = (cy_i + 1).min(cell_volume.size[1] - 1);
                    let v010 = cb[cell_volume.index(cx, next_y, cz)];
                    let v110 = cb[cell_volume.index(next_x, next_y, cz)];
                    let v011 = cb[cell_volume.index(cx, next_y, next_z)];
                    let v111 = cb[cell_volume.index(next_x, next_y, next_z)];
                    fill_cell(
                        out, v, &cell_volume, [cx, cy_i, cz], cxz, cy, inv_xz, inv_y,
                        [v000, v100, v010, v110, v001, v101, v011, v111],
                    );
                    v000 = v010;
                    v100 = v110;
                    v001 = v011;
                    v101 = v111;
                }
            }
        }
        ctx.give(cb);
    }
}

#[allow(clippy::too_many_arguments)]
fn fill_cell(out: &mut [f32], ov: &Volume, cv: &Volume, cell: [i32; 3], cxz: i32, cy: i32, inv_xz: f32, inv_y: f32, c: [f32; 8]) {
    let [v000, v100, v010, v110, v001, v101, v011, v111] = c;
    let cox = cv.block_x(cell[0]) - ov.min[0];
    let coy = cv.block_y(cell[1]) - ov.min[1];
    let coz = cv.block_z(cell[2]) - ov.min[2];
    let x0 = 0.max(-cox);
    let y0 = 0.max(-coy);
    let z0 = 0.max(-coz);
    let x1 = cxz.min(ov.size[0] - cox) - 1;
    let y1 = cy.min(ov.size[1] - coy) - 1;
    let z1 = cxz.min(ov.size[2] - coz) - 1;
    for z in z0..=z1 {
        let out_z = coz + z;
        let alpha_z = z as f32 * inv_xz;
        let v00_ = lerp(alpha_z, v000, v001);
        let v01_ = lerp(alpha_z, v010, v011);
        let v10_ = lerp(alpha_z, v100, v101);
        let v11_ = lerp(alpha_z, v110, v111);
        for x in x0..=x1 {
            let out_x = cox + x;
            let alpha_x = x as f32 * inv_xz;
            let v_0_ = lerp(alpha_x, v00_, v10_);
            let v_1_ = lerp(alpha_x, v01_, v11_);
            let step = (v_1_ - v_0_) * inv_y;
            let mut value = v_0_ + step * y0 as f32;
            let mut index = ov.index(out_x, coy + y0, out_z);
            for _ in y0..=y1 {
                out[index] = value;
                index += 1;
                value += step;
            }
        }
    }
}

fn select_index(thresholds: &[f32], count: usize, input: f32) -> usize {
    for (i, t) in thresholds.iter().enumerate() {
        if input < *t {
            return i;
        }
    }
    count - 1
}

fn unary_apply(op: UnaryOp, v: f32) -> f32 {
    match op {
        UnaryOp::Abs => v.abs(),
        UnaryOp::Square => v * v,
        UnaryOp::Cube => v * v * v,
        UnaryOp::Sqrt => v.sqrt(),
        UnaryOp::Reciprocal => 1.0 / v,
        UnaryOp::Negate => -v,
        UnaryOp::Squeeze => squeeze(v),
        UnaryOp::Log => f64::from(v).ln() as f32,
        UnaryOp::Sign => super::interval::signum(v),
    }
}

fn end_height(noise: &Simplex, section_x: i32, section_z: i32) -> f32 {
    let chunk_x = section_x / 2;
    let chunk_z = section_z / 2;
    let sub_x = section_x % 2;
    let sub_z = section_z % 2;
    let mut doffs = -100.0f32;
    for xo in -12i32..=12 {
        for zo in -12i32..=12 {
            let tx = i64::from(chunk_x) + i64::from(xo);
            let tz = i64::from(chunk_z) + i64::from(zo);
            if tx * tx + tz * tz > 4096 && noise.get(tx as f64, tz as f64) < -0.9 {
                let size = ((tx as f32).abs() * 3439.0 + (tz as f32).abs() * 147.0) % 13.0 + 9.0;
                let xd = (sub_x - xo * 2) as f32;
                let zd = (sub_z - zo * 2) as f32;
                let mut nd = 100.0 - (xd * xd + zd * zd).sqrt() * size;
                nd = clamp_f(nd, -100.0, 80.0);
                doffs = jmax(doffs, nd);
            }
        }
    }
    doffs
}

// Referenced to keep the rounding helper reachable from this module's tests.
#[allow(dead_code)]
fn _round_probe(x: f32) -> i32 {
    java_round(x)
}

impl Tiling {
    pub(crate) fn mode(self, from: i32, to: i32) -> GradientMode {
        let range = to.wrapping_sub(from);
        match self {
            Tiling::ClampToEdge => GradientMode::Clamped { min: from.min(to), max: from.max(to) },
            Tiling::Repeat => GradientMode::Repeat { range },
            Tiling::MirroredRepeat => GradientMode::Mirrored { range },
        }
    }
}

impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx").finish_non_exhaustive()
    }
}
