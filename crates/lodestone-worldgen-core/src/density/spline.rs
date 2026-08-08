//! Cubic spline evaluation for the `spline` density function.
//!
//! Reproduces `net.minecraft.util.CubicSpline` — all arithmetic in `f32`, exactly
//! as vanilla (the coordinate value is cast to `float` before sampling and every
//! interpolation is single-precision).

use super::{Context, Density};

/// A cubic spline: either a constant or a multipoint interpolation keyed by a
/// density-function coordinate.
#[derive(Debug, Clone)]
pub enum Spline {
    /// A constant spline value.
    Constant(f32),
    /// A multipoint spline.
    Multipoint {
        /// The density function whose value indexes the spline.
        coordinate: Box<Density>,
        /// The control points, ascending by `location`.
        points: Vec<SplinePoint>,
    },
}

/// A single spline control point.
#[derive(Debug, Clone)]
pub struct SplinePoint {
    /// The coordinate location of this point.
    pub location: f32,
    /// The derivative (slope) used for interpolation / linear extension.
    pub derivative: f32,
    /// The (possibly nested) spline value at this point.
    pub value: Box<Spline>,
}

impl Spline {
    /// Appends a complete, bit-exact description of this spline to `out` — see
    /// [`crate::noise::ImprovedNoise::write_signature`] for the contract, and
    /// [`Density::write_signature`] for why the node-sharing pass needs it.
    ///
    /// `f32` fields go in as `to_bits()` widened to `u64`, for the same
    /// `0.0`/`-0.0`/`NaN` reason floats never go in as compared values.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        match self {
            Spline::Constant(v) => {
                out.push(0);
                out.push(u64::from(v.to_bits()));
            }
            Spline::Multipoint { coordinate, points } => {
                out.push(1);
                coordinate.write_signature(out);
                out.push(points.len() as u64);
                for p in points {
                    out.push(u64::from(p.location.to_bits()));
                    out.push(u64::from(p.derivative.to_bits()));
                    p.value.write_signature(out);
                }
            }
        }
    }

    /// Evaluates the spline at `ctx`, in `f32` to match vanilla exactly.
    #[must_use]
    pub fn compute(&self, ctx: Context) -> f32 {
        match self {
            Spline::Constant(v) => *v,
            Spline::Multipoint { coordinate, points } => {
                let input = coordinate.compute(ctx) as f32;
                let last = points.len() - 1;
                let start = find_interval_start(points, input);
                if start < 0 {
                    return linear_extend(input, points, 0, points[0].value.compute(ctx));
                }
                let start = start as usize;
                if start == last {
                    return linear_extend(input, points, last, points[last].value.compute(ctx));
                }
                let p0 = &points[start];
                let p1 = &points[start + 1];
                let x1 = p0.location;
                let x2 = p1.location;
                let t = (input - x1) / (x2 - x1);
                let y1 = p0.value.compute(ctx);
                let y2 = p1.value.compute(ctx);
                let d1 = p0.derivative;
                let d2 = p1.derivative;
                let a = d1 * (x2 - x1) - (y2 - y1);
                let b = -d2 * (x2 - x1) + (y2 - y1);
                lerp_f32(t, y1, y2) + t * (1.0 - t) * lerp_f32(t, a, b)
            }
        }
    }
}

fn find_interval_start(points: &[SplinePoint], input: f32) -> i32 {
    // Mth.binarySearch(0, n, i -> input < locations[i]) - 1
    let mut first = points.len();
    for (i, p) in points.iter().enumerate() {
        if input < p.location {
            first = i;
            break;
        }
    }
    first as i32 - 1
}

fn linear_extend(input: f32, points: &[SplinePoint], index: usize, value: f32) -> f32 {
    let derivative = points[index].derivative;
    if derivative == 0.0 {
        value
    } else {
        value + derivative * (input - points[index].location)
    }
}

#[inline]
fn lerp_f32(t: f32, a: f32, b: f32) -> f32 {
    a + t * (b - a)
}
