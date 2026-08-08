//! `DensityFunctions.EndIslandDensityFunction` — the End's island height field.
//!
//! # What it is
//!
//! The one density-function *type* the engine's JSON interpreter still lacks, kept
//! here as a standalone, independently testable primitive rather than inside
//! `density/`. It is a `SimpleFunction`: no children, no arguments, and the JSON is
//! literally `{"type": "minecraft:end_islands"}` — the codec is
//! `MapCodec.unit(new EndIslandDensityFunction(0L))`, so the document always
//! deserialises with seed 0 and `RandomState` substitutes the real world seed
//! afterwards (`RandomState.java:74`).
//!
//! It appears **twice** in 26.2's data, not once: inline as
//! `noise_settings/end.json`'s `erosion` channel (wrapped in `cache_2d`), and
//! inside `density_function/end/sloped_cheese.json`. Anything wiring it must
//! handle both sites.
//!
//! # Why it is a separate type
//!
//! Two reasons, and the second is the useful one:
//!
//! * `crate::noise` is where the seeded-noise primitives live, and this is one —
//!   a `SimplexNoise` plus an integer height field over it.
//! * **It is independently gate-able, and `TheEndBiomeSource` needs it *without*
//!   the density interpreter.** The End's `erosion` channel is exactly
//!   `cache2d(end_islands)`, and `TheEndBiomeSource.getNoiseBiome` samples that
//!   one channel and nothing else — so the End's whole biome layout is a function
//!   of this struct alone. Coupling it to a `Density` variant would have made the
//!   biome source wait for the interpreter.
//!
//! # Its seeding does not consult `legacy_random_source`
//!
//! Worth stating because the obvious guess is wrong: `EndIslandDensityFunction`
//! always constructs `new LegacyRandomSource(seed)` (`DensityFunctions.java:498`)
//! **regardless** of the dimension's RNG-family flag. It happens that the End sets
//! the flag too, but that is a coincidence — this constructor would be the LCG
//! either way.
//!
//! # How to change it
//!
//! Every one of the following is a way to produce a plausible-looking but wrong
//! End, and each is written the way it is on purpose:
//!
//! * **`consume_count(17292)` is 17,292 discarded `nextInt()`s**, before the
//!   `SimplexNoise` constructor's own three `nextDouble`s and 256-step
//!   Fisher–Yates. Change the count and the whole archipelago moves.
//! * **`section / 2` and `block / 8` are Java *truncating* division, not
//!   floor-div.** For negative coordinates `sub_section ∈ {−1, 0, 1}`. Rust's `/`
//!   and `%` truncate identically, so they are used directly — do **not** reach
//!   for `div_euclid`.
//! * **`section_x * section_x + section_z * section_z` is computed in `i32`** and
//!   only then widened for a `f32` sqrt. The centre-hole test
//!   `total_chunk² > 4096` is separately **`i64`**.
//! * **`island_size` is a slope, not a radius**, range `[9, 22)`.
//! * **`island_size` / `xd` / `zd` / `new_doffs` are all `f32`.** `Mth.sqrt` is
//!   `(float) Math.sqrt(f)` — a `f64` sqrt narrowed back to `f32`, which is what
//!   `(x as f64).sqrt() as f32` spells.
//! * Loop bounds are `-12..=12` on both axes: **625 candidate chunks per call**,
//!   which is why the router wraps it in `cache_2d`.
//!
//! # Dependencies
//!
//! [`crate::noise::SimplexNoise`] and [`crate::rng::LegacyRandomSource`].

use crate::noise::SimplexNoise;
use crate::rng::{LegacyRandomSource, RandomSource};

/// `EndIslandDensityFunction.ISLAND_THRESHOLD`.
const ISLAND_THRESHOLD: f64 = -0.9;
/// `islandRandom.consumeCount(17292)`.
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
    /// `new EndIslandDensityFunction(seed)` (`DensityFunctions.java:496-503`).
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

    /// `getHeightValue(islandNoise, sectionX, sectionZ)` — the raw height in
    /// `[-100, 80]`, before `compute`'s offset and scale.
    ///
    /// "Section" here is vanilla's own name for an eighth of a block coordinate,
    /// not a chunk section: `compute` passes `blockX / 8`.
    #[must_use]
    pub fn height_value(&self, section_x: i32, section_z: i32) -> f32 {
        let chunk_x = section_x / 2;
        let chunk_z = section_z / 2;
        let sub_section_x = section_x % 2;
        let sub_section_z = section_z % 2;

        // `int` product first, then widened for a float sqrt.
        let radial = section_x
            .wrapping_mul(section_x)
            .wrapping_add(section_z.wrapping_mul(section_z));
        let mut doffs = (100.0 - mth_sqrt(radial as f32) * 8.0).clamp(-100.0, 80.0);

        for xo in -12i32..=12 {
            for zo in -12i32..=12 {
                let total_chunk_x = i64::from(chunk_x) + i64::from(xo);
                let total_chunk_z = i64::from(chunk_z) + i64::from(zo);
                if total_chunk_x * total_chunk_x + total_chunk_z * total_chunk_z
                    <= CENTRE_HOLE_CHUNKS_SQUARED
                {
                    continue;
                }
                if self
                    .island_noise
                    .get_value(total_chunk_x as f64, total_chunk_z as f64)
                    >= ISLAND_THRESHOLD
                {
                    continue;
                }
                let island_size =
                    ((total_chunk_x as f32).abs() * 3439.0 + (total_chunk_z as f32).abs() * 147.0)
                        % 13.0
                        + 9.0;
                let xd = (sub_section_x - xo * 2) as f32;
                let zd = (sub_section_z - zo * 2) as f32;
                let new_doffs =
                    (100.0 - mth_sqrt(xd * xd + zd * zd) * island_size).clamp(-100.0, 80.0);
                doffs = doffs.max(new_doffs);
            }
        }
        doffs
    }

    /// `compute(context)` — `(getHeightValue(blockX / 8, blockZ / 8) - 8.0) / 128.0`.
    /// `blockY` is not read, which is why the router can wrap this in `cache_2d`
    /// and why `TheEndBiomeSource` may sample it at any height.
    #[must_use]
    pub fn compute(&self, block_x: i32, block_z: i32) -> f64 {
        (f64::from(self.height_value(block_x / 8, block_z / 8)) - 8.0) / 128.0
    }

    /// `minValue()` — `(-100 - 8) / 128`.
    pub const MIN_VALUE: f64 = -0.843_75;
    /// `maxValue()` — `(80 - 8) / 128`.
    pub const MAX_VALUE: f64 = 0.562_5;
}

/// `Mth.sqrt(float)` — a **`f64`** square root narrowed back to `f32`, not
/// `f32::sqrt`. On every value this function is given the two agree, but the
/// narrowing is vanilla's own and costs nothing to reproduce.
fn mth_sqrt(v: f32) -> f32 {
    (f64::from(v)).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inside the centre hole no island can contribute, because every one of the
    /// 625 candidates fails `total_chunk² > 4096` — so the height is the closed
    /// form `clamp(100 - sqrt(sx² + sz²) * 8, -100, 80)` and nothing else.
    ///
    /// **This expectation comes from geometry, not from either implementation.**
    /// It is the [`ISLAND_THRESHOLD`] branch's own precondition evaluated by hand,
    /// which is the cross-arm-invariant shape `DESIGN.md` §12.117 established: the
    /// simplex noise is *not* consulted, so agreement here cannot be a shared
    /// misreading of the noise.
    ///
    /// **The window is smaller than it first looks**, and the premise assertion
    /// below caught the first derivation of it being wrong: the binding candidate is
    /// the *diagonal* corner `(chunk + 12, chunk + 12)`, so the condition is
    /// `2 · (|chunk| + 12)² <= 4096`, i.e. `|chunk| <= 33` and `|section| <= 67` —
    /// not the `|chunk| <= 52` that reading the radius off one axis suggests.
    /// Sampled inside `|section| <= 60`.
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

    /// Truncating division, not floor division. `blockX / 8` for `blockX = -1` is
    /// **0** in Java and in Rust, and `-1` under `div_euclid` — which would shift
    /// the entire western half of the End by one section.
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
