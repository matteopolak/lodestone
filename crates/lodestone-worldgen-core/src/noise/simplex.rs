//! `SimplexNoise` — vanilla's own simplex-noise class,
//! the 2-D-only subset [`crate::feature::vegetation`] needs for
//! vanilla's own biome-info-noise constant (grass/flower density,
//! vanilla's own noise-threshold-count placement's own flower-noise field). This is a **different**
//! noise primitive from [`super::normal::NormalNoise`]/[`super::perlin::PerlinNoise`]
//! (which back the density-function router) — vanilla keeps them as separate
//! classes with unrelated gradient tables, and so does this port.
//!
//! Vanilla's own biome-info-noise constant is constructed as a multi-octave
//! wrapper seeded from a legacy random source with seed `2345L` and a single
//! octave index `[0]` — a single-octave wrapper. For a single octave at
//! index 0, that wrapper's
//! own frequency/value scaling collapses to identity (`factor = 2^0 = 1`,
//! `valueFactor = 1/(2^1-1) = 1`), so its value at `(x, y,
//! false)` for this exact construction is bit-identical to
//! `SimplexNoise::getValue(x, y)` on the single wrapped octave — this module
//! ports only that reduced (and only ever needed) shape, not the general
//! multi-octave wrapper.

use crate::rng::RandomSource;

const GRADIENT: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, -1.0],
];

const F2: f64 = 0.5 * (1.732_050_808_568_872_4 - 1.0); // 0.5*(sqrt(3)-1)
const G2: f64 = (3.0 - 1.732_050_808_568_872_4) / 6.0;

/// A single 2-D/3-D simplex-noise octave, seeded from any [`RandomSource`].
/// Only the 2-D `get_value(x, y)` overload is ported — [`crate::feature::vegetation`]
/// (the only caller) never needs the 3-D form.
#[derive(Debug, Clone)]
pub struct SimplexNoise {
    p: [u8; 256],
}

impl SimplexNoise {
    /// Vanilla's own constructor: draws `xo`/`yo`/`zo` (three
    /// `nextDouble`s, discarded here — [`crate::feature::vegetation`] only
    /// ever calls the `useNoiseStart = false` overload, which never reads
    /// them) then Fisher-Yates shuffles the identity permutation `0..256`
    /// using `nextInt(256 - i)`, exactly as vanilla does. Vanilla backs `p`
    /// with a 512-entry array, but every read masks with `& 0xFF`, so the
    /// upper half is provably dead; this port keeps only the live 256.
    pub fn new<R: RandomSource>(random: &mut R) -> Self {
        let _xo = random.next_double() * 256.0;
        let _yo = random.next_double() * 256.0;
        let _zo = random.next_double() * 256.0;
        let mut p = [0u8; 256];
        for (i, slot) in p.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for ix in 0..256usize {
            let offset = random.next_int_bounded(256 - ix as i32) as usize;
            p.swap(ix, offset + ix);
        }
        Self { p }
    }

    /// Appends a complete, bit-exact description of this octave to `out` — see
    /// [`crate::noise::ImprovedNoise::write_signature`] for the contract.
    ///
    /// The permutation *is* the whole state (the three `nextDouble`s vanilla
    /// draws are discarded), so this is the entire struct.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        for word in self.p.chunks_exact(8) {
            out.push(u64::from_le_bytes(word.try_into().unwrap()));
        }
    }

    fn p(&self, x: i32) -> i32 {
        i32::from(self.p[(x & 0xFF) as usize])
    }

    fn corner_noise_3d(&self, index: usize, x: f64, y: f64, z: f64, base: f64) -> f64 {
        let mut t0 = base - x * x - y * y - z * z;
        if t0 < 0.0 {
            0.0
        } else {
            t0 *= t0;
            let g = GRADIENT[index];
            t0 * t0 * (g[0] * x + g[1] * y + g[2] * z)
        }
    }

    /// Vanilla's own `SimplexNoise::getValue(xin, yin)` (the 2-D overload).
    #[must_use]
    pub fn get_value(&self, xin: f64, yin: f64) -> f64 {
        let s = (xin + yin) * F2;
        let i = crate::math::floor(xin + s);
        let j = crate::math::floor(yin + s);
        let t = f64::from(i + j) * G2;
        let x0_origin = f64::from(i) - t;
        let y0_origin = f64::from(j) - t;
        let x0 = xin - x0_origin;
        let y0 = yin - y0_origin;
        let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
        let x1 = x0 - f64::from(i1) + G2;
        let y1 = y0 - f64::from(j1) + G2;
        let x2 = x0 - 1.0 + 2.0 * G2;
        let y2 = y0 - 1.0 + 2.0 * G2;
        let ii = i & 0xFF;
        let jj = j & 0xFF;
        let gi0 = (self.p(ii + self.p(jj)) % 12) as usize;
        let gi1 = (self.p(ii + i1 + self.p(jj + j1)) % 12) as usize;
        let gi2 = (self.p(ii + 1 + self.p(jj + 1)) % 12) as usize;
        let n0 = self.corner_noise_3d(gi0, x0, y0, 0.0, 0.5);
        let n1 = self.corner_noise_3d(gi1, x1, y1, 0.0, 0.5);
        let n2 = self.corner_noise_3d(gi2, x2, y2, 0.0, 0.5);
        70.0 * (n0 + n1 + n2)
    }
}
