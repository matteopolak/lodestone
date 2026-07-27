//! `ImprovedNoise` — a single 3D Perlin (improved) noise octave.
//!
//! Reproduces `net.minecraft.world.level.levelgen.synth.ImprovedNoise`: three
//! `nextDouble`-derived offsets, a Fisher–Yates permutation of `0..256` drawn
//! from the source, and the gradient-dot / trilinear-smoothstep sample. Only the
//! non-derivative `noise(x, y, z)` path (the one terrain uses) is implemented.

use crate::math::{floor, lerp3, smoothstep};
use crate::rng::RandomSource;

/// The 16 gradient vectors (`SimplexNoise.GRADIENT`), shared by the improved
/// noise sampler.
const GRADIENT: [[i32; 3]; 16] = [
    [1, 1, 0],
    [-1, 1, 0],
    [1, -1, 0],
    [-1, -1, 0],
    [1, 0, 1],
    [-1, 0, 1],
    [1, 0, -1],
    [-1, 0, -1],
    [0, 1, 1],
    [0, -1, 1],
    [0, 1, -1],
    [0, -1, -1],
    [1, 1, 0],
    [0, -1, 1],
    [-1, 1, 0],
    [0, -1, -1],
];

#[inline]
fn dot(g: [i32; 3], x: f64, y: f64, z: f64) -> f64 {
    f64::from(g[0]) * x + f64::from(g[1]) * y + f64::from(g[2]) * z
}

/// A single improved-noise octave.
#[derive(Debug, Clone)]
pub struct ImprovedNoise {
    p: [u8; 256],
    /// X offset (`nextDouble * 256`).
    pub xo: f64,
    /// Y offset.
    pub yo: f64,
    /// Z offset.
    pub zo: f64,
}

impl ImprovedNoise {
    /// Builds an octave, consuming three `nextDouble`s and a 256-step shuffle
    /// from `random` in exactly vanilla's order.
    pub fn new<R: RandomSource>(random: &mut R) -> Self {
        let xo = random.next_double() * 256.0;
        let yo = random.next_double() * 256.0;
        let zo = random.next_double() * 256.0;
        let mut p = [0u8; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for i in 0..256usize {
            let offset = random.next_int_bounded(256 - i as i32) as usize;
            p.swap(i, i + offset);
        }
        Self { p, xo, yo, zo }
    }

    #[inline]
    fn perm(&self, x: i32) -> i32 {
        i32::from(self.p[(x & 0xFF) as usize])
    }

    #[inline]
    fn grad_dot(&self, hash: i32, x: f64, y: f64, z: f64) -> f64 {
        dot(GRADIENT[(hash & 15) as usize], x, y, z)
    }

    /// Samples the noise at `(x, y, z)` (the `yScale = yFudge = 0` path).
    #[must_use]
    pub fn noise(&self, px: f64, py: f64, pz: f64) -> f64 {
        self.noise_scaled(px, py, pz, 0.0, 0.0)
    }

    /// The full `noise(x, y, z, yScale, yFudge)` used by the blended noise.
    #[must_use]
    pub fn noise_scaled(&self, px: f64, py: f64, pz: f64, y_scale: f64, y_fudge: f64) -> f64 {
        let x = px + self.xo;
        let y = py + self.yo;
        let z = pz + self.zo;
        let xf = floor(x);
        let yf = floor(y);
        let zf = floor(z);
        let xr = x - f64::from(xf);
        let yr = y - f64::from(yf);
        let zr = z - f64::from(zf);
        let yr_fudge = if y_scale != 0.0 {
            let fudge_limit = if y_fudge >= 0.0 && y_fudge < yr {
                y_fudge
            } else {
                yr
            };
            f64::from(floor(fudge_limit / y_scale + f64::from(1.0e-7_f32))) * y_scale
        } else {
            0.0
        };
        self.sample_and_lerp(xf, yf, zf, xr, yr - yr_fudge, zr, yr)
    }

    #[allow(clippy::many_single_char_names)]
    fn sample_and_lerp(
        &self,
        x: i32,
        y: i32,
        z: i32,
        xr: f64,
        yr: f64,
        zr: f64,
        yr_original: f64,
    ) -> f64 {
        let x0 = self.perm(x);
        let x1 = self.perm(x + 1);
        let xy00 = self.perm(x0 + y);
        let xy01 = self.perm(x0 + y + 1);
        let xy10 = self.perm(x1 + y);
        let xy11 = self.perm(x1 + y + 1);
        let d000 = self.grad_dot(self.perm(xy00 + z), xr, yr, zr);
        let d100 = self.grad_dot(self.perm(xy10 + z), xr - 1.0, yr, zr);
        let d010 = self.grad_dot(self.perm(xy01 + z), xr, yr - 1.0, zr);
        let d110 = self.grad_dot(self.perm(xy11 + z), xr - 1.0, yr - 1.0, zr);
        let d001 = self.grad_dot(self.perm(xy00 + z + 1), xr, yr, zr - 1.0);
        let d101 = self.grad_dot(self.perm(xy10 + z + 1), xr - 1.0, yr, zr - 1.0);
        let d011 = self.grad_dot(self.perm(xy01 + z + 1), xr, yr - 1.0, zr - 1.0);
        let d111 = self.grad_dot(self.perm(xy11 + z + 1), xr - 1.0, yr - 1.0, zr - 1.0);
        let x_alpha = smoothstep(xr);
        let y_alpha = smoothstep(yr_original);
        let z_alpha = smoothstep(zr);
        lerp3(
            x_alpha, y_alpha, z_alpha, d000, d100, d010, d110, d001, d101, d011, d111,
        )
    }
}
