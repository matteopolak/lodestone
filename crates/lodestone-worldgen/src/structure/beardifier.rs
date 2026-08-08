//! The beardifier — issue #514's S3, and the only part of structure generation
//! that changes *terrain* rather than adding blocks to it.
//!
//! # What it is
//!
//! Vanilla's `Beardifier` (`world/level/levelgen/Beardifier.java`). A density
//! term, added to `final_density` at every block of a chunk, that raises the
//! density under and around an adaptation-bearing structure's pieces so the
//! terrain grows a flat foundation ("beard") or swallows the piece whole
//! ("bury"). Without it a village sits draped over whatever hillside the noise
//! happened to produce.
//!
//! # How it works
//!
//! Two things have to be right, and they are at opposite ends of the pipeline.
//!
//! **Where the term is added.** `NoiseChunk`'s constructor
//! (`NoiseChunk.java:155-160`) does not read `final_density` directly:
//!
//! ```text
//! fullNoiseValue = DensityFunctions.cacheAllInCell(
//!         DensityFunctions.add(wrappedRouter.finalDensity(), BeardifierMarker.INSTANCE))
//!     .mapAll(this::wrap);
//! ```
//!
//! and `NoiseChunk.wrap` substitutes the real `Beardifier` for the marker. So the
//! beard is **one operand of a plain `add` at the very top of the graph**, and
//! `Ap2(ADD)` is `argument1.compute(ctx) + argument2.compute(ctx)`. That is why
//! this type does not live inside the density evaluator at all: adding it at the
//! `final_density` *call site* is the same floating-point expression, in the same
//! order, and it keeps a per-chunk mutable input out of a graph that is shared
//! across threads by `Arc`.
//!
//! It also explains something that reads as a missing feature: the string
//! `beardifier` appears **nowhere** in 26.2's shipped worldgen JSON (checked
//! across `.cache/mc/26.2` — the only hit in the whole tree is
//! `registries.json`'s type list). `Density::Beardifier` and
//! `OpKind::Beardifier` therefore parse and evaluate to `0.0` and are *correct*
//! to do so: the marker is only reachable from data a pack author wrote, and the
//! overworld's own adaptation comes from the code-level `add` above. Anyone
//! chasing "why is `OpKind::Beardifier => 0.0`" should stop here.
//!
//! **Which pieces are in scope.** `Beardifier.forStructuresInChunk` takes the
//! chunk's `References` starts with `terrainAdaptation() != NONE`, then keeps
//! each *piece* within 12 blocks of the chunk. [`super::super::overworld::structures::StructureRefs`]
//! already computes the start-level set (deliberately *wider* than vanilla's, so
//! one product serves both the beardifier and the persistence view), and the
//! piece-level filter here re-narrows it to exactly vanilla's set. That the two
//! agree is not an accident of tuning: a piece within 12 blocks of the chunk
//! implies the start's 12-inflated box intersects the chunk box, which is
//! precisely vanilla's `createReferences` test, so "piece within 12" is the
//! binding condition either way.
//!
//! # How to change it, and the gotchas
//!
//! * **[`Beardifier::EMPTY`] is not an optimisation, it is the correctness
//!   story for every chunk in the world.** `affected_box == None` short-circuits
//!   to `0.0`, and [`OverworldGenerator::fill_stage`](crate::overworld::OverworldGenerator)
//!   goes further and skips the addition *entirely* for an empty beardifier, so a
//!   chunk with no adaptation-bearing start in reach runs byte-identical code to
//!   the pre-S3 pipeline. `x + 0.0` is `x` for every finite `x`, but it is *not*
//!   `x` for `x == -0.0`, and the skip means that question never arises.
//! * **[`fast_inv_sqrt`] must stay bit-exact.** It is Newton's method off a magic
//!   integer, not `1.0 / sqrt(x)`; the two differ in the 11th significant digit,
//!   and the beard shape is the difference between a flat plaza and a lumpy one.
//! * **`ground_level_delta` is a jigsaw fact, and until S4 lands every piece
//!   reports `0`** — vanilla's own answer for a non-`PoolElementStructurePiece`
//!   (`Beardifier.java:75`). [`PieceBeard`] is the seam: give a piece `Some(..)`
//!   and its rigid/junction behaviour switches on.
//! * The kernel is 13,824 `f32`s built once. It is keyed `[zi][xi][yi]` — **Y
//!   innermost**, which is not the order the loop that builds it reads, so the
//!   index expression is `zi * 576 + xi * 24 + yi` and transposing it produces a
//!   plausible-looking but wrong beard.
//!
//! # Dependencies
//!
//! [`BoundingBox`] and [`TerrainAdjustment`] from [`super`], and nothing else —
//! no noise, no RNG, no resolver. Every value it returns is a pure function of
//! the piece geometry, which is what lets it be unit-tested against hand-expanded
//! arithmetic from the record definition.

use std::sync::OnceLock;

use super::{BoundingBox, StructureStart, TerrainAdjustment};

/// `Beardifier.BEARD_KERNEL_RADIUS` — also the amount
/// `Structure.adjustBoundingBox` inflates an adaptation-bearing box by, and the
/// piece-level `isCloseToChunk` distance. One number, three uses, all the same
/// reason.
pub const BEARD_KERNEL_RADIUS: i32 = 12;

/// `Beardifier.BEARD_KERNEL_SIZE`.
const BEARD_KERNEL_SIZE: i32 = 24;

/// How far past the union of the in-scope pieces the beard can reach —
/// `anyPieceBoundingBox.inflatedBy(24)`.
const AFFECTED_INFLATION: i32 = 24;

/// `Beardifier.BEARD_KERNEL`: `exp(-|(dx, dy + 0.5, dz)|² / 16)` over the
/// 24×24×24 offset cube, as `f32`, indexed `[zi * 576 + xi * 24 + yi]`.
///
/// Built once per process rather than per chunk. `f32` because vanilla's array
/// is `float[]` and the product in [`beard_contribution`] promotes it back to
/// `double` — storing `f64` here would keep precision vanilla throws away.
fn kernel() -> &'static [f32; 13824] {
    static KERNEL: OnceLock<[f32; 13824]> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let mut k = [0.0f32; 13824];
        for zi in 0..BEARD_KERNEL_SIZE {
            for xi in 0..BEARD_KERNEL_SIZE {
                for yi in 0..BEARD_KERNEL_SIZE {
                    let idx = (zi * BEARD_KERNEL_SIZE * BEARD_KERNEL_SIZE
                        + xi * BEARD_KERNEL_SIZE
                        + yi) as usize;
                    k[idx] = compute_beard_contribution(
                        xi - BEARD_KERNEL_RADIUS,
                        f64::from(yi - BEARD_KERNEL_RADIUS) + 0.5,
                        zi - BEARD_KERNEL_RADIUS,
                    ) as f32;
                }
            }
        }
        k
    })
}

/// `Beardifier.computeBeardContribution(int, double, int)`.
///
/// `E.powf(..)` rather than `(..).exp()` to mirror vanilla's
/// `Math.pow(Math.E, x)` as an expression. The two agree to within an ulp and the
/// result is immediately narrowed to `f32`, so the distinction is almost never
/// observable — but "almost never" is exactly the kind of claim this repo has
/// been burnt by, so the spelling matches.
fn compute_beard_contribution(dx: i32, dy: f64, dz: i32) -> f64 {
    let distance_sqr = length_squared(f64::from(dx), dy, f64::from(dz));
    std::f64::consts::E.powf(-distance_sqr / 16.0)
}

/// `Mth.lengthSquared(x, y, z)`.
fn length_squared(x: f64, y: f64, z: f64) -> f64 {
    x * x + y * y + z * z
}

/// `Mth.length(x, y, z)`.
fn length(x: f64, y: f64, z: f64) -> f64 {
    length_squared(x, y, z).sqrt()
}

/// `Mth.fastInvSqrt` (`Mth.java:449-455`) — one Newton step from the classic
/// magic-constant seed.
///
/// **Not** `1.0 / x.sqrt()`, and the difference is real: the relative error of
/// this is ~1e-11, not 1e-16. It is the only transcendental-adjacent step in the
/// beard, so a "cleaner" rewrite changes every beard value in the world.
#[must_use]
fn fast_inv_sqrt(x: f64) -> f64 {
    let xhalf = 0.5 * x;
    let i = 6_910_469_410_427_058_090_i64.wrapping_sub(x.to_bits() as i64 >> 1);
    let x = f64::from_bits(i as u64);
    x * (1.5 - xhalf * x * x)
}

/// `Beardifier.getBuryContribution` — a linear falloff from 1 at the piece to 0
/// at distance 6.
fn bury_contribution(dx: f64, dy: f64, dz: f64) -> f64 {
    crate::math::clamped_map(length(dx, dy, dz), 0.0, 6.0, 1.0, 0.0)
}

/// `Beardifier.getBeardContribution` — the kernel sample, signed by how far the
/// query sits above the piece's ground level.
fn beard_contribution(dx: i32, dy: i32, dz: i32, y_to_ground: i32) -> f64 {
    let xi = dx + BEARD_KERNEL_RADIUS;
    let yi = dy + BEARD_KERNEL_RADIUS;
    let zi = dz + BEARD_KERNEL_RADIUS;
    if !in_kernel_range(xi) || !in_kernel_range(yi) || !in_kernel_range(zi) {
        return 0.0;
    }
    let dy_with_offset = f64::from(y_to_ground) + 0.5;
    let distance_sqr = length_squared(f64::from(dx), dy_with_offset, f64::from(dz));
    let value = -dy_with_offset * fast_inv_sqrt(distance_sqr / 2.0) / 2.0;
    let idx = (zi * BEARD_KERNEL_SIZE * BEARD_KERNEL_SIZE + xi * BEARD_KERNEL_SIZE + yi) as usize;
    value * f64::from(kernel()[idx])
}

fn in_kernel_range(i: i32) -> bool {
    (0..BEARD_KERNEL_SIZE).contains(&i)
}

/// A jigsaw junction — `JigsawJunction`, the point where two template pool
/// pieces meet.
///
/// Each one contributes its own soft beard at 0.4 weight, which is what smooths
/// the ground *between* a village's houses rather than only under them. Produced
/// by S4; nothing constructs one yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Junction {
    /// `getSourceX`.
    pub source_x: i32,
    /// `getSourceGroundY`.
    pub source_ground_y: i32,
    /// `getSourceZ`.
    pub source_z: i32,
}

/// The `PoolElementStructurePiece`-only facts the beardifier reads.
///
/// A piece with `None` here is vanilla's `else` branch (`Beardifier.java:75`): it
/// beards as a rigid box with `groundLevelDelta == 0` and contributes no
/// junctions. That is the correct answer for every coded piece, so this stays
/// `Option` rather than gaining a "not jigsaw" variant.
#[derive(Debug, Clone, Default)]
pub struct PieceBeard {
    /// Whether the pool element's `projection` is `rigid`. A
    /// `terrain_matching` element is **excluded from the rigid list entirely** —
    /// it follows the terrain instead of flattening it — but its junctions still
    /// count.
    pub rigid: bool,
    /// `getGroundLevelDelta` — how far above the piece's `minY` its own floor
    /// sits, from the template's `groundLevelDelta` marker.
    pub ground_level_delta: i32,
    /// `getJunctions`.
    pub junctions: Vec<Junction>,
}

/// One `Beardifier.Rigid`: a piece box, its structure's adjustment, and its
/// ground-level delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rigid {
    box_: BoundingBox,
    adjustment: TerrainAdjustment,
    ground_level_delta: i32,
}

/// One chunk's beard term.
///
/// Build with [`for_chunk`](Self::for_chunk); query with
/// [`compute`](Self::compute). [`is_empty`](Self::is_empty) is the fast path the
/// fill stage branches on — see this module's doc for why that branch is a
/// correctness property and not a micro-optimisation.
#[derive(Debug, Clone, Default)]
pub struct Beardifier {
    pieces: Vec<Rigid>,
    junctions: Vec<Junction>,
    /// `null` in vanilla; `None` here. Its absence is what makes
    /// [`Self::compute`] a constant `0.0`.
    affected_box: Option<BoundingBox>,
}

impl Beardifier {
    /// `Beardifier.EMPTY` — no pieces, no junctions, and therefore a constant
    /// `0.0`.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// `Beardifier.forStructuresInChunk`, over the starts
    /// [`StructureRefs::adaptation_bearing`](crate::overworld::structures::StructureRefs::adaptation_bearing)
    /// already filtered.
    ///
    /// `starts` must already be adaptation-bearing; this does not re-check, for
    /// the same reason vanilla passes the predicate to `startsForStructure`
    /// rather than testing inside the loop — the filter belongs to the reference
    /// walk, which is where it can be paid for once per chunk instead of once per
    /// piece.
    #[must_use]
    pub fn for_chunk<'a>(
        cx: i32,
        cz: i32,
        starts: impl Iterator<Item = &'a StructureStart>,
    ) -> Self {
        let chunk_start_block_x = cx * 16;
        let chunk_start_block_z = cz * 16;
        let mut pieces = Vec::new();
        let mut junctions = Vec::new();
        let mut any: Option<BoundingBox> = None;

        for start in starts {
            let adjustment = start.terrain_adaptation;
            for piece in &start.pieces {
                if !piece
                    .bounding_box
                    .is_close_to_chunk(cx, cz, BEARD_KERNEL_RADIUS)
                {
                    continue;
                }
                match &piece.beard {
                    Some(jigsaw) => {
                        // A `terrain_matching` element contributes no rigid box
                        // *and* does not widen `affected_box` — only its
                        // junctions do. Collapsing the two into one `if` is the
                        // easy mistake here.
                        if jigsaw.rigid {
                            pieces.push(Rigid {
                                box_: piece.bounding_box,
                                adjustment,
                                ground_level_delta: jigsaw.ground_level_delta,
                            });
                            any = Some(include(any, piece.bounding_box));
                        }
                        for junction in &jigsaw.junctions {
                            // Strict inequalities, and the window is the chunk
                            // plus 12 on each side — vanilla's
                            // `Beardifier.java:66-70`.
                            if junction.source_x > chunk_start_block_x - BEARD_KERNEL_RADIUS
                                && junction.source_z > chunk_start_block_z - BEARD_KERNEL_RADIUS
                                && junction.source_x
                                    < chunk_start_block_x + 15 + BEARD_KERNEL_RADIUS
                                && junction.source_z
                                    < chunk_start_block_z + 15 + BEARD_KERNEL_RADIUS
                            {
                                junctions.push(*junction);
                                any = Some(include(
                                    any,
                                    BoundingBox::of_block(
                                        junction.source_x,
                                        junction.source_ground_y,
                                        junction.source_z,
                                    ),
                                ));
                            }
                        }
                    }
                    None => {
                        pieces.push(Rigid {
                            box_: piece.bounding_box,
                            adjustment,
                            ground_level_delta: 0,
                        });
                        any = Some(include(any, piece.bounding_box));
                    }
                }
            }
        }

        let Some(any) = any else {
            return Self::empty();
        };
        Self {
            pieces,
            junctions,
            affected_box: Some(any.inflated_by(AFFECTED_INFLATION)),
        }
    }

    /// Whether this beardifier is `Beardifier.EMPTY`-equivalent — a constant
    /// `0.0` at every position.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.affected_box.is_none()
    }

    /// How many rigid pieces contribute. Exposed for gates that need to say
    /// *what* they measured rather than only that something changed.
    #[must_use]
    pub fn rigid_count(&self) -> usize {
        self.pieces.len()
    }

    /// How many junctions contribute.
    #[must_use]
    pub fn junction_count(&self) -> usize {
        self.junctions.len()
    }

    /// The box outside which this beardifier is identically `0.0`, or `None` when
    /// it is `0.0` everywhere.
    #[must_use]
    pub fn affected_box(&self) -> Option<BoundingBox> {
        self.affected_box
    }

    /// `Beardifier.compute` — the density term to add to `final_density` at this
    /// block.
    ///
    /// Accumulation order is the specification: rigids in insertion order, then
    /// junctions in insertion order, each `+=` in `f64`. Summing them in any
    /// other order is a different number.
    #[must_use]
    pub fn compute(&self, x: i32, y: i32, z: i32) -> f64 {
        let Some(affected) = self.affected_box else {
            return 0.0;
        };
        if !contains(affected, x, y, z) {
            return 0.0;
        }

        let mut noise = 0.0f64;
        for rigid in &self.pieces {
            let b = rigid.box_;
            let dx = 0.max((b.min[0] - x).max(x - b.max[0]));
            let dz = 0.max((b.min[2] - z).max(z - b.max[2]));
            let ground_y = b.min[1] + rigid.ground_level_delta;
            let dy_to_ground = y - ground_y;

            let dy = match rigid.adjustment {
                TerrainAdjustment::None => 0,
                TerrainAdjustment::Bury | TerrainAdjustment::BeardThin => dy_to_ground,
                TerrainAdjustment::BeardBox => 0.max((ground_y - y).max(y - b.max[1])),
                TerrainAdjustment::Encapsulate => 0.max((b.min[1] - y).max(y - b.max[1])),
            };

            noise += match rigid.adjustment {
                TerrainAdjustment::None => 0.0,
                TerrainAdjustment::Bury => bury_contribution(
                    f64::from(dx),
                    f64::from(dy) / 2.0,
                    f64::from(dz),
                ),
                TerrainAdjustment::BeardThin | TerrainAdjustment::BeardBox => {
                    beard_contribution(dx, dy, dz, dy_to_ground) * 0.8
                }
                TerrainAdjustment::Encapsulate => {
                    bury_contribution(
                        f64::from(dx) / 2.0,
                        f64::from(dy) / 2.0,
                        f64::from(dz) / 2.0,
                    ) * 0.8
                }
            };
        }

        for junction in &self.junctions {
            let dx = x - junction.source_x;
            let dy = y - junction.source_ground_y;
            let dz = z - junction.source_z;
            noise += beard_contribution(dx, dy, dz, dy) * 0.4;
        }

        noise
    }
}

/// `Beardifier.includeBoundingBox`.
fn include(encompassing: Option<BoundingBox>, new: BoundingBox) -> BoundingBox {
    match encompassing {
        None => new,
        Some(existing) => existing.encapsulate(new),
    }
}

/// `BoundingBox.isInside(x, y, z)`.
fn contains(b: BoundingBox, x: i32, y: i32, z: i32) -> bool {
    x >= b.min[0]
        && x <= b.max[0]
        && y >= b.min[1]
        && y <= b.max[1]
        && z >= b.min[2]
        && z <= b.max[2]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structure::StructurePiece;

    fn piece(box_: BoundingBox) -> StructurePiece {
        StructurePiece {
            id: "minecraft:test".to_string(),
            bounding_box: box_,
            orientation: None,
            gen_depth: 0,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: None,
            loot: Vec::new(),
            beard: None,
        }
    }

    fn start(adjustment: TerrainAdjustment, box_: BoundingBox) -> StructureStart {
        StructureStart {
            structure: "minecraft:test".to_string(),
            chunk_x: 0,
            chunk_z: 0,
            references: 0,
            bounding_box: box_,
            pieces: vec![piece(box_)],
            terrain_adaptation: adjustment,
            pieces_complete: true,
        }
    }

    /// `Mth.fastInvSqrt` against values hand-carried through the record
    /// definition (`Mth.java:449-455`): seed = `longBitsToDouble(6910469410427058090 -
    /// (doubleToRawLongBits(x) >> 1))`, then one Newton step.
    ///
    /// The point of the test is the **gap from the exact answer**, because that
    /// gap is the whole reason the function exists. `1.0 / x.sqrt()` would pass a
    /// tolerance-based check and fail this one.
    #[test]
    fn fast_inv_sqrt_is_newton_not_exact() {
        // Values from expanding `Mth.java:449-455` on the raw bit patterns — the
        // integer subtract, the reinterpret, and the one Newton step — independent
        // of this file. Full `f64` precision, so a one-ulp drift fails.
        let expected: [(f64, f64); 8] = [
            (0.125, 2.827_718_603_181_855_5),
            (0.5, 1.413_859_301_590_927_8),
            (1.0, 0.998_308_142_711_814_5),
            (2.0, 0.706_929_650_795_463_9),
            (8.0, 0.353_464_825_397_731_94),
            (72.5, 0.117_435_552_939_170_38),
            (1024.0, 0.031_197_129_459_744_2),
            (12345.678, 0.008_992_565_706_850_876),
        ];
        for (x, want) in expected {
            assert_eq!(fast_inv_sqrt(x), want, "x={x}");
        }
        // And it is genuinely *not* `1.0 / sqrt(x)`. The single Newton step leaves
        // up to ~1.7e-3 of relative error — three orders of magnitude more than a
        // rounding difference, and the exact reason the exact-value assertions
        // above cannot be replaced by a tolerance.
        let worst = expected
            .iter()
            .map(|&(x, _)| ((fast_inv_sqrt(x) - 1.0 / x.sqrt()) / (1.0 / x.sqrt())).abs())
            .fold(0.0f64, f64::max);
        assert!(
            (1e-4..2e-3).contains(&worst),
            "the approximation error should be ~1e-3, measured {worst:e}: \
             a much smaller value means someone replaced this with 1/sqrt"
        );
    }

    /// The kernel's own definition, at three offsets, computed from
    /// `exp(-|(dx, dy + 0.5, dz)|² / 16)` by hand rather than by calling the
    /// function under test.
    #[test]
    fn kernel_matches_its_definition() {
        let k = kernel();
        let at = |dx: i32, dy: i32, dz: i32| {
            let (xi, yi, zi) = (dx + 12, dy + 12, dz + 12);
            k[(zi * 576 + xi * 24 + yi) as usize]
        };
        for (dx, dy, dz) in [(0, 0, 0), (1, -2, 3), (-11, 11, -11)] {
            let d2 = f64::from(dx * dx) + (f64::from(dy) + 0.5).powi(2) + f64::from(dz * dz);
            let want = (-d2 / 16.0).exp() as f32;
            let got = at(dx, dy, dz);
            assert!(
                (f64::from(got) - f64::from(want)).abs() <= f64::from(f32::EPSILON),
                "({dx},{dy},{dz}): want {want}, got {got}"
            );
        }
        // The index order is `[zi][xi][yi]`, Y innermost. A transposed kernel is
        // symmetric in x/z and would pass every value check above, so assert the
        // asymmetry the +0.5 y-offset creates: (0, +1, 0) and (0, -1, 0) are
        // *different*, while (+1, 0, 0) and (0, 0, +1) are the same.
        assert_ne!(at(0, 1, 0), at(0, -1, 0), "the y offset must be +0.5");
        assert_eq!(at(1, 0, 0), at(0, 0, 1), "x and z are symmetric");
    }

    /// An empty beardifier is `0.0` everywhere, and a start whose pieces are all
    /// out of reach produces one.
    #[test]
    fn out_of_reach_is_empty() {
        let far = BoundingBox::from_corners([200, 60, 200], [210, 70, 210]);
        let b = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::BeardThin, far)].iter());
        assert!(b.is_empty());
        assert_eq!(b.compute(8, 64, 8), 0.0);
        assert!(Beardifier::empty().is_empty());
    }

    /// `bury` at the piece's own ground level is the maximum contribution, 1.0,
    /// and falls linearly to 0 at distance 6 — `clampedMap(length, 0, 6, 1, 0)`
    /// expanded by hand.
    #[test]
    fn bury_falls_off_linearly_to_six() {
        let b = BoundingBox::from_corners([0, 64, 0], [0, 64, 0]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::Bury, b)].iter());
        assert!(!beard.is_empty());
        assert_eq!(beard.rigid_count(), 1);

        // At the block itself: dx = dy = dz = 0, length 0, clampedMap -> 1.0.
        assert!((beard.compute(0, 64, 0) - 1.0).abs() < 1e-12);
        // 3 blocks away horizontally: length 3, clampedMap(3, 0, 6, 1, 0) = 0.5.
        assert!((beard.compute(3, 64, 0) - 0.5).abs() < 1e-12);
        // Bury halves the *vertical* distance, so 6 blocks up is dy/2 = 3 -> 0.5.
        assert!((beard.compute(0, 70, 0) - 0.5).abs() < 1e-12);
        // Past 6 the clamp holds it at exactly 0, not merely small.
        assert_eq!(beard.compute(7, 64, 0), 0.0);
    }

    /// `beard_thin`'s sign is the whole point: **below** a piece's ground level
    /// the term is positive (fill in a foundation), **above** it is negative
    /// (shave the hillside off). A magnitude-only assertion would pass with the
    /// sign inverted, and an inverted beard digs a pit under every village.
    #[test]
    fn beard_thin_fills_below_and_cuts_above() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 70, 7]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::BeardThin, b)].iter());

        let below = beard.compute(3, 60, 3);
        let above = beard.compute(3, 68, 3);
        assert!(below > 0.0, "below the floor must fill: {below}");
        assert!(above < 0.0, "above the floor must cut: {above}");

        // The exact value at the floor itself, hand-expanded: dx = dz = 0,
        // dy_to_ground = 0, so dy_with_offset = 0.5, distance_sqr = 0.25,
        // value = -0.5 * fastInvSqrt(0.125) / 2, times kernel[(0, 0, 0)] and 0.8.
        let dy_with_offset = 0.5f64;
        let value = -dy_with_offset * fast_inv_sqrt(0.25 / 2.0) / 2.0;
        let want = value * f64::from((-0.25f64 / 16.0).exp() as f32) * 0.8;
        let got = beard.compute(0, 64, 0);
        assert!(
            (got - want).abs() < 1e-15,
            "at the floor: want {want}, got {got}"
        );
    }

    /// `encapsulate` uses `bury`'s falloff on **halved** offsets and scales by
    /// 0.8, and its `dy` is measured from the box rather than from the ground
    /// level. Two structures differ only by this arm (`trial_chambers` vs
    /// `trail_ruins`), so mixing them up is invisible without a direct check.
    #[test]
    fn encapsulate_halves_every_offset() {
        let b = BoundingBox::from_corners([0, 64, 0], [0, 64, 0]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::Encapsulate, b)].iter());
        // 6 blocks away: dx/2 = 3, length 3 -> clampedMap 0.5, times 0.8.
        assert!((beard.compute(6, 64, 0) - 0.4).abs() < 1e-12);
        // Inside the box, every offset is 0 -> 1.0 * 0.8.
        assert!((beard.compute(0, 64, 0) - 0.8).abs() < 1e-12);
    }

    /// `beard_box` clamps `dy` to zero *inside* the piece's vertical span, so the
    /// whole box interior gets the strongest beard rather than a gradient. That
    /// is the difference between the ancient city's flat floor and a bowl.
    #[test]
    fn beard_box_clamps_dy_inside_the_span() {
        let b = BoundingBox::from_corners([0, 64, 0], [0, 80, 0]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::BeardBox, b)].iter());
        // At y = 70, inside [64, 80]: max(0, max(64 - 70, 70 - 80)) = 0, but
        // `dy_to_ground` is still 6, so the *sign* comes from the ground delta
        // while the kernel index comes from the clamped dy.
        let inside = beard.compute(0, 70, 0);
        let want = {
            let dy_with_offset = 6.0f64 + 0.5;
            let d2 = dy_with_offset * dy_with_offset;
            let value = -dy_with_offset * fast_inv_sqrt(d2 / 2.0) / 2.0;
            value * f64::from((-(0.5f64 * 0.5) / 16.0).exp() as f32) * 0.8
        };
        assert!(
            (inside - want).abs() < 1e-15,
            "beard_box inside the span: want {want}, got {inside}"
        );
    }

    /// `terrain_adaptation: none` contributes exactly nothing even when its box
    /// is in reach — which is why the reference walk filters it out entirely and
    /// this arm is only ever reached through a hand-built beardifier.
    #[test]
    fn none_contributes_zero() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 70, 7]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::None, b)].iter());
        assert!(!beard.is_empty(), "the box still widens `affected_box`");
        assert_eq!(beard.compute(3, 64, 3), 0.0);
    }

    /// A junction contributes at 0.4 weight, and the window that admits it is
    /// **strict** on all four sides — a junction exactly 12 blocks outside the
    /// chunk is excluded.
    #[test]
    fn junction_window_is_strict() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 70, 7]);
        let mut s = start(TerrainAdjustment::BeardThin, b);
        s.pieces[0].beard = Some(PieceBeard {
            rigid: true,
            ground_level_delta: 0,
            junctions: vec![
                // Inside: x = -11 > 0 - 12.
                Junction {
                    source_x: -11,
                    source_ground_y: 64,
                    source_z: 4,
                },
                // Exactly on the boundary: x = -12 is *not* > -12.
                Junction {
                    source_x: -12,
                    source_ground_y: 64,
                    source_z: 4,
                },
                // Exactly on the far boundary: x = 27 is *not* < 15 + 12.
                Junction {
                    source_x: 27,
                    source_ground_y: 64,
                    source_z: 4,
                },
            ],
        });
        let beard = Beardifier::for_chunk(0, 0, [s].iter());
        assert_eq!(beard.junction_count(), 1);
        assert_eq!(beard.rigid_count(), 1);
    }

    /// A `terrain_matching` pool element contributes **no** rigid box, and does
    /// not widen `affected_box` either. Both halves of that matter: a village's
    /// paths are terrain-matching, and bearding them would flatten the roads.
    #[test]
    fn terrain_matching_contributes_no_rigid() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 70, 7]);
        let mut s = start(TerrainAdjustment::BeardThin, b);
        s.pieces[0].beard = Some(PieceBeard {
            rigid: false,
            ground_level_delta: 0,
            junctions: Vec::new(),
        });
        let beard = Beardifier::for_chunk(0, 0, [s].iter());
        assert_eq!(beard.rigid_count(), 0);
        assert!(
            beard.is_empty(),
            "with no rigid box and no junction there is nothing to affect"
        );
    }

    /// `ground_level_delta` moves the *sign flip* up, not the box. A piece whose
    /// floor is 3 blocks above its `minY` must fill below y = minY + 3 and cut
    /// above it.
    #[test]
    fn ground_level_delta_moves_the_sign_flip() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 80, 7]);
        let mut s = start(TerrainAdjustment::BeardThin, b);
        s.pieces[0].beard = Some(PieceBeard {
            rigid: true,
            ground_level_delta: 3,
            junctions: Vec::new(),
        });
        let beard = Beardifier::for_chunk(0, 0, [s].iter());
        assert!(beard.compute(3, 66, 3) > 0.0, "below minY + 3 fills");
        assert!(beard.compute(3, 68, 3) < 0.0, "above minY + 3 cuts");
    }

    /// The affected box is the piece union inflated by 24, and `compute` is
    /// exactly `0.0` outside it — the short-circuit the fill stage's byte
    /// identity depends on.
    #[test]
    fn affected_box_is_the_union_inflated_by_24() {
        let b = BoundingBox::from_corners([0, 64, 0], [7, 70, 7]);
        let beard = Beardifier::for_chunk(0, 0, [start(TerrainAdjustment::BeardThin, b)].iter());
        let affected = beard.affected_box().expect("non-empty");
        assert_eq!(affected.min, [-24, 40, -24]);
        assert_eq!(affected.max, [31, 94, 31]);
        assert_eq!(beard.compute(-25, 64, 0), 0.0);
        assert_eq!(beard.compute(0, 95, 0), 0.0);
    }
}
