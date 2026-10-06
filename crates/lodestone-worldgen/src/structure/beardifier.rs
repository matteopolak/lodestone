//! Piece selection for the beardifier, the one part of structure generation
//! that changes terrain rather than adding blocks to it.
//!
//! # What it is
//!
//! The beardifier is a density term added to the final density at every block
//! of a chunk. It raises the density under and around an adaptation-bearing
//! structure's pieces, so the terrain grows a flat foundation ("beard") or
//! swallows the piece ("bury"). Without it a village sits draped over whatever
//! hillside the noise produced.
//!
//! # How it works
//!
//! [`Beardifier::for_chunk`] takes the adaptation-bearing starts that
//! [`StructureRefs`](crate::structure::chunk::StructureRefs) gathered for a
//! chunk and keeps each piece within [`BEARD_KERNEL_RADIUS`] blocks of it. A
//! rigid piece contributes its box; a jigsaw piece also contributes its
//! junctions inside the chunk plus 12 on each side. The 26.3 engine does the
//! arithmetic: [`Beardifier::to_release26_3`] converts the selection, and
//! `Terrain263::beardifier_for` hands it to the density fill.
//!
//! The start-level reference set is deliberately wider than the piece-level
//! one, so one product serves both the beardifier and persistence. The two
//! agree: a piece within 12 blocks of the chunk implies its start's
//! 12-inflated box intersects the chunk.
//!
//! # How to change it, and the gotchas
//!
//! * **A `terrain_matching` jigsaw element contributes junctions but no rigid
//!   box, and does not widen the affected box.** Collapsing those two into one
//!   condition is the easy mistake.
//! * **The junction window uses strict inequalities.**
//! * **An empty beardifier must stay exactly zero.** `x + 0.0` is not `x` for
//!   `x == -0.0`, so the conversion of an empty selection is a term that is zero
//!   everywhere, not a skipped addition with different rounding.
//!
//! # Dependencies
//!
//! [`BoundingBox`] and [`TerrainAdjustment`] from [`super`], and the 26.3
//! engine's own beardifier in `lodestone-worldgen-core`. No noise, no RNG, no
//! resolver.

use super::{BoundingBox, StructureStart, TerrainAdjustment};

/// Vanilla's own beard-kernel radius — also the amount
/// vanilla's own bounding-box adjustment inflates an adaptation-bearing box by, and the
/// piece-level "close to chunk" distance. One number, three uses, all the same
/// reason.
pub const BEARD_KERNEL_RADIUS: i32 = 12;

/// How far past the union of the in-scope pieces the beard can reach —
/// any piece's own bounding box inflated by 24.
const AFFECTED_INFLATION: i32 = 24;

/// A jigsaw junction — vanilla's own jigsaw-junction type, the point where two template pool
/// pieces meet.
///
/// Each one contributes its own soft beard at 0.4 weight, which is what smooths
/// the ground *between* a village's houses rather than only under them. Jigsaw
/// assembly records one per connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Junction {
    /// Vanilla's own source-x accessor.
    pub source_x: i32,
    /// Vanilla's own source-ground-y accessor.
    pub source_ground_y: i32,
    /// Vanilla's own source-z accessor.
    pub source_z: i32,
}

/// The jigsaw-piece-only facts the beardifier reads.
///
/// A piece with `None` here is vanilla's own `else` branch for a non-jigsaw
/// piece: it
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
    /// Vanilla's own ground-level-delta accessor — how far above the piece's `minY` its own floor
    /// sits, from the template's `groundLevelDelta` marker.
    pub ground_level_delta: i32,
    /// Vanilla's own junctions accessor.
    pub junctions: Vec<Junction>,
}

/// Vanilla's own rigid-piece record: a piece box, its structure's adjustment, and its
/// ground-level delta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rigid {
    box_: BoundingBox,
    adjustment: TerrainAdjustment,
    ground_level_delta: i32,
}

/// One chunk's beard term.
///
/// Build with [`for_chunk`](Self::for_chunk); convert with
/// [`to_release26_3`](Self::to_release26_3).
#[derive(Debug, Clone, Default)]
pub struct Beardifier {
    pieces: Vec<Rigid>,
    junctions: Vec<Junction>,
    /// `None` when no piece or junction is in reach: the term is then zero
    /// everywhere.
    affected_box: Option<BoundingBox>,
}

impl Beardifier {
    /// Vanilla's own empty-beardifier singleton — no pieces, no junctions, and therefore a constant
    /// `0.0`.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Vanilla's own "beardifier for structures in chunk" constructor, over the starts
    /// [`StructureRefs::adaptation_bearing`](crate::structure::chunk::StructureRefs::adaptation_bearing)
    /// already filtered.
    ///
    /// `starts` must already be adaptation-bearing; this does not re-check, for
    /// the same reason vanilla passes the predicate to its own
    /// starts-for-structure lookup
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
                            // plus 12 on each side — vanilla's own junction
                            // in-range test.
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

    /// Whether this beardifier is equivalent to vanilla's own empty singleton — a constant
    /// `0.0` at every position.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.affected_box.is_none()
    }
}

impl Beardifier {
    /// The same in-reach pieces and junctions as the 32-bit 26.3 terrain term.
    ///
    /// Piece selection stays here; the arithmetic is the 26.3 engine's, which
    /// accumulates in `f32`. An empty beardifier converts to one that is zero everywhere.
    #[must_use]
    pub fn to_release26_3(&self) -> lodestone_worldgen_core::engine::release26_3::beardifier::Beardifier {
        use lodestone_worldgen_core::engine::release26_3::beardifier as core;
        let adjustment = |a: TerrainAdjustment| match a {
            TerrainAdjustment::None => core::TerrainAdjustment::None,
            TerrainAdjustment::Bury => core::TerrainAdjustment::Bury,
            TerrainAdjustment::BeardThin => core::TerrainAdjustment::BeardThin,
            TerrainAdjustment::BeardBox => core::TerrainAdjustment::BeardBox,
            TerrainAdjustment::Encapsulate => core::TerrainAdjustment::Encapsulate,
        };
        let pieces = self
            .pieces
            .iter()
            .map(|r| core::Rigid {
                bounds: core::BoundingBox { min: r.box_.min, max: r.box_.max },
                adjustment: adjustment(r.adjustment),
                ground_level_delta: r.ground_level_delta,
            })
            .collect();
        let junctions = self
            .junctions
            .iter()
            .map(|j| core::Junction { source_x: j.source_x, source_ground_y: j.source_ground_y, source_z: j.source_z })
            .collect();
        core::Beardifier::new(pieces, junctions)
    }
}

/// Vanilla's own include-bounding-box helper.
fn include(encompassing: Option<BoundingBox>, new: BoundingBox) -> BoundingBox {
    match encompassing {
        None => new,
        Some(existing) => existing.encapsulate(new),
    }
}

