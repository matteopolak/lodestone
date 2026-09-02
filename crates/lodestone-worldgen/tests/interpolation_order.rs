//! Which of vanilla's **two** trilinear interpolation orders the block field
//! uses — and a standing guard against "simplifying" to the other one.
//!
//! # The trap
//!
//! Vanilla's own noise-chunk-sampler's noise-interpolator has two value paths, and they are different
//! floating-point expressions over the same eight cell corners:
//!
//! | vanilla path | expression | nesting |
//! |---|---|---|
//! | `fillingCell == true` | vanilla's own math-helper trilinear lerp, whose bilinear half is `lerp(dy, lerp(dx, x00, x10), lerp(dx, x01, x11))` | **X inner**, then Y, then Z |
//! | `fillingCell == false` | the incremental `updateForY` → `updateForX` → `updateForZ` chain | **Y inner**, then X, then Z |
//!
//! Bilinear interpolation is order-independent *algebraically* and is **not**
//! order-independent in IEEE 754. Both orders exist in the real jar, so "which
//! one does the per-block field use" is a question that has to be answered from
//! the source, not assumed — and the plausible-looking answer is the wrong one.
//!
//! # The answer, and why it is not the obvious one
//!
//! The per-block field uses **vanilla's own math-helper trilinear lerp (X inner)**, which is what
//! `lodestone-worldgen-core`'s `density/chunk.rs` implements.
//!
//! The reason is two levels removed from the interpolator. Vanilla's own
//! noise-chunk-sampler's
//! constructor does not read the router's
//! `final_density` directly — it wraps it: it builds a plain two-argument `add`
//! between the wrapped router's own final-density function and a
//! beardifier-marker leaf, caches that sum over the whole cell, and then
//! re-maps every leaf of the resulting graph through its own wrap step.
//!
//! That `cache_all_in_cell` is applied **in code, not in data** — `grep` finds
//! no `minecraft:cache_all_in_cell` anywhere in the 26.2 worldgen JSON, so
//! reading the `noise_settings` document alone cannot see it. Its cell array is
//! pre-filled inside vanilla's own "select cell YZ" step, which brackets
//! the fill with `fillingCell = true` / `fillingCell = false`. So every value
//! vanilla's own interpolated-density accessor ever returns for `final_density` was produced in
//! the `fillingCell == true` regime — i.e. by vanilla's own math-helper
//! trilinear lerp. The incremental
//! `updateForY/X/Z` chain, despite being the machinery the driver loop appears
//! to be feeding, is **never** what `final_density` reads.
//!
//! # Why this test exists rather than a comment
//!
//! Because the two orders differ by only ~1 ULP, the wrong choice does not look
//! wrong: it survives every smoke test and most of a chunk. Measured on this
//! tree, swapping `density/chunk.rs` to the incremental chain took
//! `chunk_parity`'s whole-chunk JVM gate from **98304/98304** to
//! **90563/98304** — 7,741 diverged blocks, every one a 1-ULP difference. That
//! is a real regression that a reader following vanilla's *driver loop* rather
//! than its *cache wrapping* would introduce while believing they were porting
//! the algorithm more faithfully. `docs/plans/worldgen-rewrite.md`'s U4 row
//! prescribes exactly that ("vanilla's incremental cell walk"), so the trap is
//! written into the plan.
//!
//! This test therefore asserts the two things `chunk_parity` cannot:
//!
//! 1. the two nestings are **bit-distinguishable on real router data** —
//!    measured **60,300 of 393,216** blocks over four chunk/seed cases, worst
//!    absolute difference `1.78e-15` — so their equivalence may never be
//!    assumed (and if this ever stops holding, the guard says so rather than
//!    passing vacuously); and
//! 2. production computes the **X-inner** one.
//!
//! `chunk_parity` proves the composed field against the real JVM. This proves
//! *which expression* produces it, which is the fact a rewrite needs.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{Builder, NoiseChunkSampler, NoiseParams, Resolver};
use serde_json::Value;

const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().expect("firstOctave") as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .expect("amplitudes")
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }
}

#[inline]
fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// Vanilla's own math-helper trilinear lerp — X innermost, then Y, then Z. Vanilla's `fillingCell` path, and
/// what `density/chunk.rs` implements.
#[allow(clippy::too_many_arguments)]
fn nesting_x_inner(
    fx: f64,
    fy: f64,
    fz: f64,
    n000: f64,
    n100: f64,
    n010: f64,
    n110: f64,
    n001: f64,
    n101: f64,
    n011: f64,
    n111: f64,
) -> f64 {
    let z0 = lerp(fy, lerp(fx, n000, n100), lerp(fx, n010, n110));
    let z1 = lerp(fy, lerp(fx, n001, n101), lerp(fx, n011, n111));
    lerp(fz, z0, z1)
}

/// `NoiseInterpolator`'s incremental chain — Y innermost, then X, then Z. The
/// order the driver loop *appears* to produce, and the one `final_density` never
/// actually reads.
#[allow(clippy::too_many_arguments)]
fn nesting_y_inner(
    fx: f64,
    fy: f64,
    fz: f64,
    n000: f64,
    n100: f64,
    n010: f64,
    n110: f64,
    n001: f64,
    n101: f64,
    n011: f64,
    n111: f64,
) -> f64 {
    // updateForY
    let value_xz00 = lerp(fy, n000, n010);
    let value_xz10 = lerp(fy, n100, n110);
    let value_xz01 = lerp(fy, n001, n011);
    let value_xz11 = lerp(fy, n101, n111);
    // updateForX
    let value_z0 = lerp(fx, value_xz00, value_xz10);
    let value_z1 = lerp(fx, value_xz01, value_xz11);
    // updateForZ
    lerp(fz, value_z0, value_z1)
}

struct Harvest {
    corners: Vec<f64>,
    ny: usize,
    nz: usize,
}

impl Harvest {
    #[inline]
    fn get(&self, ix: usize, iy: usize, iz: usize) -> f64 {
        self.corners[(ix * self.ny + iy) * self.nz + iz]
    }
}

/// Reads the whole 5 × 49 × 5 corner lattice for chunk `(cx, cz)` out of
/// `sampler` by querying it at corner positions only.
///
/// This works because at a cell corner every lerp factor is exactly `0.0` and
/// `lerp(0.0, a, b) == a + 0.0 * (b - a) == a` exactly, so the sampler's own
/// interpolation is the identity there. No private access, and no second
/// implementation of corner evaluation to get wrong.
fn harvest_corners(sampler: &NoiseChunkSampler, cx: i32, cz: i32) -> Harvest {
    let base_x = cx * 16;
    let base_z = cz * 16;
    let nx = (16 / CELL_WIDTH + 1) as usize; // 5
    let nz = (16 / CELL_WIDTH + 1) as usize; // 5
    let ny = (HEIGHT / CELL_HEIGHT + 1) as usize; // 49
    let mut corners = vec![0.0; nx * ny * nz];
    for ix in 0..nx {
        for iy in 0..ny {
            for iz in 0..nz {
                let x = base_x + ix as i32 * CELL_WIDTH;
                let y = MIN_Y + iy as i32 * CELL_HEIGHT;
                let z = base_z + iz as i32 * CELL_WIDTH;
                corners[(ix * ny + iy) * nz + iz] = sampler.final_density(x, y, z);
            }
        }
    }
    Harvest { corners, ny, nz }
}

/// A sampler whose **root** is the overworld router's one `interpolated` node.
///
/// This indirection is load-bearing, and the first version of this test got it
/// wrong. `noise_router.final_density` is `min(squeeze(interpolated(...)), ...)`,
/// so the interpolated marker is nested two levels down and the surrounding
/// `squeeze`/`min` are applied at the block position — they vary *within* a
/// cell. Sampling the router root at a cell corner therefore does not yield a
/// corner value, and the corner-harvest premise holds only for a root that *is*
/// the marker. The control below caught the first attempt: it reported
/// 178,815 / 393,216 blocks unexplained.
///
/// Extracting the subtree changes no seeding: `Builder` instantiates noises via
/// `master.from_hash_of(id)`, keyed by registry id alone, so a subtree built
/// standalone carries bit-identical noise instances. Cache-slot indices
/// renumber, and a slot index is a cache address, never a value.
fn interpolated_sampler_for(seed: i64, root: &Path) -> NoiseChunkSampler {
    let resolver = FsResolver {
        root: root.to_path_buf(),
    };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let node = &settings["noise_router"]["final_density"]["argument1"]["argument"];
    assert_eq!(
        node["type"].as_str(),
        Some("minecraft:interpolated"),
        "the overworld router's shape moved: expected final_density.argument1.argument \
         to be the interpolated marker, found {:?}. This test's corner-harvest premise \
         depends on rooting the sampler at that marker.",
        node["type"]
    );
    let builder = Builder::new(seed, &resolver);
    let interpolated = builder.build(node);
    NoiseChunkSampler::new(interpolated, builder.slot_count(), CELL_WIDTH, CELL_HEIGHT)
}

/// The measurement, over whole chunks at three seeds.
///
/// * **Control** — the harvested lattice recomputed with the X-inner nesting
///   must reproduce `NoiseChunkSampler::final_density` bit-for-bit. This proves
///   both that the harvest is real and that production is X-inner. It is a real
///   control, not a described one: it fired on the first (wrongly-rooted)
///   version of this test.
/// * **Anti-vacuity** — most of the sample must have both `fx` and `fy`
///   non-zero, since `lerp(0.0, a, b) == a` makes the two nestings trivially
///   equal otherwise. Without this the test could pass by measuring nothing.
/// * **The guard** — the two nestings must actually *differ* somewhere. If they
///   ever stop differing, the distinction this test exists to protect is
///   unobservable here and the reader needs to know that rather than inherit a
///   silently vacuous guard.
#[test]
fn block_field_uses_mth_lerp3_nesting_not_the_incremental_chain() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    // Three independent seeds, and chunks away from the origin so the lattice is
    // not the all-corners-near-zero special case.
    let cases: [(i64, i32, i32); 4] = [(42, 0, 0), (42, 7, -3), (1234, 0, 0), (987_654_321, 2, 5)];

    let mut checked = 0usize;
    let mut control_mismatch = 0usize;
    let mut order_differences = 0usize;
    let mut non_trivial = 0usize;
    let mut worst_abs_diff = 0.0f64;

    for (seed, cx, cz) in cases {
        let sampler = interpolated_sampler_for(seed, &root);
        let h = harvest_corners(&sampler, cx, cz);
        let base_x = cx * 16;
        let base_z = cz * 16;

        for lx in 0..16i32 {
            for lz in 0..16i32 {
                for ly in 0..HEIGHT {
                    let x = base_x + lx;
                    let y = MIN_Y + ly;
                    let z = base_z + lz;

                    let ix = (lx / CELL_WIDTH) as usize;
                    let iy = (ly / CELL_HEIGHT) as usize;
                    let iz = (lz / CELL_WIDTH) as usize;

                    let n000 = h.get(ix, iy, iz);
                    let n100 = h.get(ix + 1, iy, iz);
                    let n010 = h.get(ix, iy + 1, iz);
                    let n110 = h.get(ix + 1, iy + 1, iz);
                    let n001 = h.get(ix, iy, iz + 1);
                    let n101 = h.get(ix + 1, iy, iz + 1);
                    let n011 = h.get(ix, iy + 1, iz + 1);
                    let n111 = h.get(ix + 1, iy + 1, iz + 1);

                    let fx = f64::from(lx % CELL_WIDTH) / f64::from(CELL_WIDTH);
                    let fy = f64::from(ly % CELL_HEIGHT) / f64::from(CELL_HEIGHT);
                    let fz = f64::from(lz % CELL_WIDTH) / f64::from(CELL_WIDTH);

                    if fx != 0.0 && fy != 0.0 {
                        non_trivial += 1;
                    }

                    let x_inner =
                        nesting_x_inner(fx, fy, fz, n000, n100, n010, n110, n001, n101, n011, n111);
                    let y_inner =
                        nesting_y_inner(fx, fy, fz, n000, n100, n010, n110, n001, n101, n011, n111);
                    let live = sampler.final_density(x, y, z);

                    checked += 1;
                    if x_inner.to_bits() != live.to_bits() {
                        control_mismatch += 1;
                    }
                    if x_inner.to_bits() != y_inner.to_bits() {
                        order_differences += 1;
                        worst_abs_diff = worst_abs_diff.max((x_inner - y_inner).abs());
                    }
                }
            }
        }
    }

    println!("interpolation-nesting characterisation over {checked} blocks (4 chunk/seed cases):");
    println!("  non-trivial blocks (fx != 0 && fy != 0):     {non_trivial}");
    println!("  blocks where X-inner != Y-inner:             {order_differences}");
    println!("  worst |x_inner - y_inner|:                   {worst_abs_diff:e}");

    assert_eq!(
        control_mismatch, 0,
        "the harvested lattice recomputed with the X-inner (Mth.lerp3) nesting failed to \
         reproduce NoiseChunkSampler::final_density on {control_mismatch}/{checked} blocks. \
         Either the corner-harvest premise is false (so this test measures nothing), or \
         density/chunk.rs no longer implements Mth.lerp3 — which is a parity change: \
         vanilla reads final_density through a code-level cache_all_in_cell whose prefill \
         runs with fillingCell == true, and that path is Mth.lerp3. See this module's doc."
    );

    assert!(
        non_trivial > checked / 2,
        "only {non_trivial}/{checked} blocks had both fx and fy non-zero; the guard below \
         would be satisfied by a lerp(0.0, a, b) == a tautology"
    );

    assert!(
        order_differences > 0,
        "the X-inner (Mth.lerp3) and Y-inner (incremental) nestings agreed bit-for-bit on \
         all {checked} blocks. That makes this test's guard vacuous: it can no longer tell \
         a correct port from one that follows vanilla's driver loop instead of its cache \
         wrapping. Investigate before deleting — the distinction was worth 7741 diverged \
         blocks of chunk_parity when it was measured."
    );

    // Both nestings are the same bilinear function, so every difference must be
    // last-place rounding. Bounded in *absolute* terms rather than in ULPs
    // deliberately: an interpolated density passes through zero, and two values
    // straddling zero are thousands of ULPs apart while being ~1e-15 apart, so a
    // ULP bound here reports a scary number for the healthy case (measured: 2048
    // ULPs, 1.78e-15 absolute). What this needs to catch is one of the two helpers
    // above being miswritten, which would differ by O(1), not by O(1e-16).
    assert!(
        worst_abs_diff < 1e-9,
        "the two nestings differed by {worst_abs_diff:e} somewhere, which is far more than \
         last-place rounding between two orderings of the same bilinear expression — one of \
         nesting_x_inner/nesting_y_inner is miswritten, so neither the control nor the guard \
         above is measuring what it claims"
    );
}
