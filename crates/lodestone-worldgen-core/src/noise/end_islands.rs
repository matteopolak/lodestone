//! The End's seeded island height field, consumed by density and biome sampling.
//!
//! The erosion channel and `density_function/end/sloped_cheese.json` share this
//! primitive. Seeding always uses the legacy stream with 17,292 discarded rounds.
//! Coordinate division truncates toward zero; the radial square wraps in `i32`,
//! while the center-hole predicate uses `i64`. Candidate distances and slopes
//! use `f32`, with square roots evaluated in `f64` and narrowed before multiplying.
//! Preserve these operations and the x-major candidate order when changing it.

use crate::noise::SimplexNoise;
use crate::rng::{LegacyRandomSource, RandomSource};

/// Only simplex samples below this value admit an outer-island center.
const ISLAND_THRESHOLD: f64 = -0.9;
/// Discarded legacy stream rounds before constructing the simplex permutation.
const CONSUMED_ROUNDS: u32 = 17_292;
/// The centre hole, in **chunks squared**: within radius 64 of the origin no
/// island ever spawns, which is what leaves the main island's plateau (produced by
/// the unconditional first `doffs` term) unbroken.
const CENTRE_HOLE_CHUNKS_SQUARED: i64 = 4096;

/// The End's island height field.
#[derive(Debug, Clone)]
pub struct EndIslandNoise {
    island_noise: SimplexNoise,
}

impl EndIslandNoise {
    /// Construct the seeded island field.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        let mut random = LegacyRandomSource::new(seed);
        random.consume_count(CONSUMED_ROUNDS);
        Self {
            island_noise: SimplexNoise::new(&mut random),
        }
    }

    /// Appends a complete, bit-exact description of this noise to `out` — see
    /// [`crate::noise::ImprovedNoise::write_signature`] for the contract.
    ///
    /// Needed because `engine::graph`'s node-sharing pass keys its leaf table on
    /// this: the End's `erosion` channel and `end/sloped_cheese.json` both reach
    /// `end_islands`, so without a signature the two occurrences compile to two
    /// leaves holding two copies of the same 256-byte permutation.
    pub fn write_signature(&self, out: &mut Vec<u64>) {
        self.island_noise.write_signature(out);
    }

    /// Raw height before the affine density transform. Each input unit is
    /// eight blocks; it is unrelated to a vertical chunk section.
    #[must_use]
    pub fn height_value(&self, section_x: i32, section_z: i32) -> f32 {
        Self::height_value_with_noise(section_x, section_z, |x, z| {
            self.island_noise.get_value(x, z)
        })
    }

    fn height_value_with_noise(
        section_x: i32,
        section_z: i32,
        mut sample: impl FnMut(f64, f64) -> f64,
    ) -> f32 {
        let chunk_x = section_x / 2;
        let chunk_z = section_z / 2;
        let sub_section_x = section_x % 2;
        let sub_section_z = section_z % 2;

        // `int` product first, then widened for a float sqrt.
        let radial = section_x
            .wrapping_mul(section_x)
            .wrapping_add(section_z.wrapping_mul(section_z));
        let mut doffs = (100.0 - mth_sqrt(radial as f32) * 8.0).clamp(-100.0, 80.0);
        if doffs == 80.0 {
            return doffs;
        }

        for xo in -12i32..=12 {
            for zo in -12i32..=12 {
                let total_chunk_x = i64::from(chunk_x) + i64::from(xo);
                let total_chunk_z = i64::from(chunk_z) + i64::from(zo);
                if total_chunk_x * total_chunk_x + total_chunk_z * total_chunk_z
                    <= CENTRE_HOLE_CHUNKS_SQUARED
                {
                    continue;
                }
                let xd = (sub_section_x - xo * 2) as f32;
                let zd = (sub_section_z - zo * 2) as f32;
                let distance_squared = xd * xd + zd * zd;
                if candidate_cannot_raise(distance_squared, doffs) {
                    continue;
                }
                if !(sample(total_chunk_x as f64, total_chunk_z as f64) < ISLAND_THRESHOLD) {
                    continue;
                }
                let island_size =
                    ((total_chunk_x as f32).abs() * 3439.0 + (total_chunk_z as f32).abs() * 147.0)
                        % 13.0
                        + 9.0;
                let new_doffs =
                    (100.0 - mth_sqrt(distance_squared) * island_size).clamp(-100.0, 80.0);
                doffs = doffs.max(new_doffs);
            }
        }
        doffs
    }

    /// Sample `(x / 8, z / 8)` and transform the height by `(value - 8) / 128`.
    /// The field is independent of Y.
    #[must_use]
    pub fn compute(&self, block_x: i32, block_z: i32) -> f64 {
        (f64::from(self.height_value(block_x / 8, block_z / 8)) - 8.0) / 128.0
    }

    /// Lower clamp limit after the affine density transform.
    pub const MIN_VALUE: f64 = -0.843_75;
    /// Upper clamp limit after the affine density transform.
    pub const MAX_VALUE: f64 = 0.562_5;
}

/// A square root evaluated in `f64` and narrowed before subsequent arithmetic.
fn mth_sqrt(v: f32) -> f32 {
    (f64::from(v)).sqrt() as f32
}

#[inline]
fn candidate_cannot_raise(distance_squared: f32, current: f32) -> bool {
    // Distances have exact integer squares at most 1250, and slopes are at
    // least nine. The height allowance exceeds the sqrt cast, multiply and
    // subtraction rounding error; strict rejection also preserves zero ties.
    let reach = 100.0 - f64::from(current) + 1.0 / 1024.0;
    current.is_finite() && 81.0 * f64::from(distance_squared) > reach * reach
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar_reference(noise: &EndIslandNoise, sx: i32, sz: i32) -> (f32, usize) {
        let radial = sx.wrapping_mul(sx).wrapping_add(sz.wrapping_mul(sz));
        let mut height = (100.0 - (f64::from(radial as f32)).sqrt() as f32 * 8.0)
            .clamp(-100.0, 80.0);
        let mut samples = 0;
        for dx in -12i32..=12 {
            for dz in -12i32..=12 {
                let cx = i64::from(sx / 2) + i64::from(dx);
                let cz = i64::from(sz / 2) + i64::from(dz);
                if cx * cx + cz * cz <= 4096 {
                    continue;
                }
                samples += 1;
                if !(noise.island_noise.get_value(cx as f64, cz as f64) < -0.9) {
                    continue;
                }
                let slope = ((cx as f32).abs() * 3439.0 + (cz as f32).abs() * 147.0)
                    % 13.0 + 9.0;
                let x = (sx % 2 - dx * 2) as f32;
                let z = (sz % 2 - dz * 2) as f32;
                let distance = f64::from(x * x + z * z).sqrt() as f32;
                height = height.max((100.0 - distance * slope).clamp(-100.0, 80.0));
            }
        }
        (height, samples)
    }

    #[test]
    fn geometric_rejection_has_arithmetic_and_aggressive_bound_controls() {
        // A 3-4-5 triangle at the minimum slope contributes exactly 55.
        let contribution = 100.0_f32 - 5.0 * 9.0;
        assert_eq!(contribution, 55.0);
        assert!(candidate_cannot_raise(25.0, 56.0));
        assert!(!candidate_cannot_raise(25.0, 55.0));
        assert!(!candidate_cannot_raise(25.0, 54.0));
        // Using the maximum slope would discard this strictly winning center.
        let wrong_upper = 100.0_f32 - 5.0 * 22.0;
        assert!(wrong_upper < 54.0 && contribution > 54.0);
        assert_ne!(wrong_upper.max(54.0).to_bits(), contribution.max(54.0).to_bits());
        assert!(!candidate_cannot_raise(25.0, f32::NAN));
        assert!(!candidate_cannot_raise(25.0, f32::INFINITY));
        assert!(!candidate_cannot_raise(0.0, -0.0));
    }

    #[test]
    fn geometric_rejection_skips_noise_for_plateau_and_mixed_residues() {
        let noise = EndIslandNoise::new(42);
        for (sx, sz) in [(65_536, -65_536), (801, -803), (-801, 803)] {
            let (reference, reference_samples) = scalar_reference(&noise, sx, sz);
            let mut samples = 0;
            let height = EndIslandNoise::height_value_with_noise(sx, sz, |x, z| {
                samples += 1;
                noise.island_noise.get_value(x, z)
            });
            assert_eq!(height.to_bits(), reference.to_bits(), "({sx}, {sz})");
            assert_eq!(reference_samples, 625);
            assert!(samples < reference_samples, "({sx}, {sz}): {samples} samples");
            if sx == 65_536 {
                // Both squared terms wrap to zero, so the independent plateau is 80.
                assert_eq!(height, 80.0);
                assert_eq!(samples, 0);
            } else {
                assert_eq!((sx % 2, sz % 2), (sx.signum(), sz.signum()));
                assert!(samples > 0, "mixed-sign island control must sample noise");
            }
        }
    }

    #[test]
    fn geometric_rejection_matches_scalar_bits_on_sampled_coordinates() {
        let coordinates = [
            (800, 802), (800, 803), (800, -803),
            (801, 802), (801, 803), (801, -803),
            (-801, 802), (-801, 803), (-801, -803),
            (0, 0), (65_536, -65_536), (46_341, 1),
            (i32::MIN, i32::MAX), (137, -129), (-67, 67), (-1, 1),
        ];
        for seed in [-195_764_831, 42] {
            let noise = EndIslandNoise::new(seed);
            for (sx, sz) in coordinates {
                let (reference, _) = scalar_reference(&noise, sx, sz);
                assert_eq!(
                    noise.height_value(sx, sz).to_bits(), reference.to_bits(),
                    "seed {seed}, ({sx}, {sz})",
                );
            }
        }
    }

    /// When all centers lie inside radius 64, only the radial term contributes.
    /// The diagonal window corners bound that premise, independently of noise.
    #[test]
    fn inside_the_centre_hole_the_height_is_the_closed_form_plateau() {
        let noise = EndIslandNoise::new(-195_764_831);
        let mut checked = 0usize;
        for section_x in [-60i32, -32, -9, -1, 0, 1, 7, 60] {
            for section_z in [-60i32, -32, -9, -1, 0, 1, 7, 60] {
                // The precondition, re-derived rather than assumed: every
                // candidate chunk must be inside radius 64.
                let chunk_x = section_x / 2;
                let chunk_z = section_z / 2;
                let all_inside = (-12i32..=12).all(|xo| {
                    (-12i32..=12).all(|zo| {
                        let tx = i64::from(chunk_x + xo);
                        let tz = i64::from(chunk_z + zo);
                        tx * tx + tz * tz <= CENTRE_HOLE_CHUNKS_SQUARED
                    })
                });
                assert!(
                    all_inside,
                    "({section_x},{section_z}) is not fully inside the centre hole; \
                     this test's premise would be false there"
                );
                let radial = section_x * section_x + section_z * section_z;
                let expected = (100.0f32 - (f64::from(radial as f32)).sqrt() as f32 * 8.0)
                    .clamp(-100.0, 80.0);
                assert_eq!(
                    noise.height_value(section_x, section_z).to_bits(),
                    expected.to_bits(),
                    "({section_x},{section_z})"
                );
                checked += 1;
            }
        }
        assert_eq!(checked, 64);
    }

    /// The control for the test above: outside the hole the field must *not* be
    /// that closed form, or the plateau test is measuring a function that has no
    /// island term at all. Observed, not described.
    #[test]
    fn outside_the_centre_hole_islands_actually_raise_the_field() {
        let noise = EndIslandNoise::new(-195_764_831);
        let mut raised = 0usize;
        let mut sampled = 0usize;
        // Far from the origin the first term saturates at -100, so any value above
        // it can only have come from an island.
        for section_x in (2000..3000).step_by(37) {
            for section_z in (2000..3000).step_by(37) {
                sampled += 1;
                if noise.height_value(section_x, section_z) > -100.0 {
                    raised += 1;
                }
            }
        }
        assert!(
            raised > 0,
            "no island contributed anywhere in {sampled} samples far from the \
             origin: the -12..=12 loop or the threshold is not firing at all"
        );
        assert!(
            raised < sampled,
            "every one of {sampled} far samples was raised: the centre-hole or \
             threshold test is not rejecting anything"
        );
    }

    /// `compute` is `(height - 8) / 128`, so the declared bounds are exactly the
    /// two `clamp` limits mapped through it — arithmetic, not a measurement — and
    /// no sample may escape them.
    #[test]
    fn compute_is_the_affine_map_of_the_height_field_and_respects_its_bounds() {
        assert_eq!(EndIslandNoise::MIN_VALUE, (-100.0 - 8.0) / 128.0);
        assert_eq!(EndIslandNoise::MAX_VALUE, (80.0 - 8.0) / 128.0);
        let noise = EndIslandNoise::new(42);
        for block_x in (-40_000..40_000).step_by(4_099) {
            for block_z in (-40_000..40_000).step_by(6_101) {
                let got = noise.compute(block_x, block_z);
                assert_eq!(
                    got,
                    (f64::from(noise.height_value(block_x / 8, block_z / 8)) - 8.0) / 128.0
                );
                assert!(
                    (EndIslandNoise::MIN_VALUE..=EndIslandNoise::MAX_VALUE).contains(&got),
                    "({block_x},{block_z}) = {got} escapes [{}, {}]",
                    EndIslandNoise::MIN_VALUE,
                    EndIslandNoise::MAX_VALUE
                );
            }
        }
    }

    /// Negative coordinates retain truncating division and signed remainders.
    #[test]
    fn negative_coordinates_truncate_toward_zero() {
        assert_eq!(-1i32 / 8, 0);
        assert_eq!(-9i32 / 8, -1);
        assert_eq!(-1i32 % 2, -1);
        assert_eq!(-3i32 / 2, -1);
        // And the sub-section really does take the value −1 there, which the
        // `xd`/`zd` terms then use.
        let noise = EndIslandNoise::new(7);
        assert!(noise.height_value(-1, 0).is_finite());
    }

    /// Two independently constructed noises must agree exactly — the determinism
    /// rule, and the reason `consume_count` and the shuffle cannot be reordered.
    #[test]
    fn independent_constructions_agree_bit_for_bit() {
        let a = EndIslandNoise::new(-195_764_831);
        let b = EndIslandNoise::new(-195_764_831);
        for (x, z) in [(0, 0), (137, -244), (-4001, 9), (100_000, -100_000)] {
            assert_eq!(a.compute(x, z).to_bits(), b.compute(x, z).to_bits());
        }
    }

    /// A different seed must give a different archipelago, or the 17,292-round
    /// consume is not reaching the shuffle.
    #[test]
    fn the_seed_reaches_the_island_field() {
        let a = EndIslandNoise::new(1);
        let b = EndIslandNoise::new(2);
        let differs = (2000..2400)
            .step_by(7)
            .any(|s| a.height_value(s, s).to_bits() != b.height_value(s, s).to_bits());
        assert!(differs, "two seeds produced the same island field");
    }
}
