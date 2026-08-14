//! The beardifier changes **terrain** (issue #514's S3), and a chunk with no
//! adaptation-bearing start nearby does not change at all.
//!
//! # Why both arms are here
//!
//! S3 is the one structures phase that edits the density field rather than adding
//! blocks on top of it, so it is the one that can silently perturb the entire
//! world. That makes the negative control the load-bearing half, not the
//! decoration: **a chunk with no beard must produce the same bytes it did before
//! S3 existed.**
//!
//! Three arms, and each answers something the others structurally cannot:
//!
//! | arm | question | how it can fail |
//! |---|---|---|
//! | [`a_beard_raises_terrain_under_the_piece`] | does a beard reach blocks at all? | the term is computed and discarded |
//! | [`the_beard_is_local_to_its_affected_box`] | does it reach only where it should? | a global offset, which a "terrain changed" assertion cannot see |
//! | [`no_start_means_no_beard_and_no_change`] | is an ordinary chunk untouched? | the fill takes the beard branch everywhere |
//!
//! The second is the one that matters most and it is why this file measures **by
//! location**: a gate reporting only "N blocks differ" cannot tell a beard from a
//! uniform density shift, and a uniform shift is exactly what a wrong operand
//! order or a missing `affected_box` check produces.
//!
//! # Where the expected values come from
//!
//! Not from our own generator. The *arithmetic* is hand-expanded from
//! `Beardifier.java` in `crates/lodestone-worldgen/src/structure/beardifier.rs`'s
//! own unit tests (`bury` falling linearly to 0 at distance 6, `beard_thin`
//! flipping sign at the piece's ground level, and the exact kernel product at the
//! floor). What this file adds is the *consequence*: that those numbers, added to
//! `final_density`, move the solid/air boundary in the direction and only in the
//! region the record says.
//!
//! The start is **synthetic**, and that is a deliberate consequence of the phase
//! order rather than a shortcut. Every one of 26.2's seven adaptation-bearing
//! structures is jigsaw (S4) or coded (S5): `ancient_city`, `pillager_outpost`,
//! `trail_ruins`, `trial_chambers` and the five villages are jigsaw; `stronghold`
//! and `nether_fossil` are coded. So no *real* start can carry a beard until one
//! of those phases lands, and a gate that waited for one would leave S3's terrain
//! maths ungated in the meantime. The seed and chunk are still taken from the
//! vanilla-authored oracle world, so the terrain the beard acts on is not
//! arbitrary.

use lodestone_worldgen::aquifer::BlockKind;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::overworld::OverworldGenerator;
use lodestone_worldgen::structure::beardifier::{Beardifier, PieceBeard};
use lodestone_worldgen::structure::{
    BoundingBox, StructurePiece, StructureStart, TerrainAdjustment,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The survival oracle world's seed — `.cache/mc/survival/world`, vanilla
/// authored. Used so the terrain the beard reshapes is a real world's, and so a
/// failure here is comparable with S1's and S2's gates.
const SEED: i64 = -195_764_831;

/// A land chunk of that world. Land matters: the beard has to be measured against
/// a solid column, and an ocean column's solid top sits far below the water line
/// where a small density change moves nothing visible.
const CHUNK: (i32, i32) = (18, 24);

/// A [`Resolver`] over `crates/lodestone-server/assets/worldgen` — the same JSON
/// the integrated server embeds. No structure sets are served: this file supplies
/// its own start, so a real one would only add noise.
struct Assets(PathBuf);

impl Assets {
    fn new() -> Self {
        Self(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../lodestone-server/assets/worldgen"),
        )
    }

    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.0.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for Assets {
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
    fn biome_parameters(&self) -> Value {
        self.read("biome_parameters", "overworld")
    }
    fn biome_temperatures(&self) -> Value {
        self.read("biome_parameters", "overworld_temperature")
    }
}

fn generator() -> OverworldGenerator {
    let assets = Assets::new();
    let settings = assets.read("noise_settings", "overworld");
    OverworldGenerator::new(SEED, &settings, &assets, "minecraft:plains", false)
}

/// A one-piece start with the given adjustment over `box_`.
fn synthetic_start(adjustment: TerrainAdjustment, box_: BoundingBox) -> StructureStart {
    StructureStart {
        structure: "minecraft:test_beard".to_string(),
        chunk_x: box_.min[0] >> 4,
        chunk_z: box_.min[2] >> 4,
        references: 0,
        bounding_box: box_,
        pieces: vec![StructurePiece {
            id: "minecraft:test_beard_piece".to_string(),
            bounding_box: box_,
            orientation: None,
            gen_depth: 0,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: None,
            loot: Vec::new(),
            beard: Some(PieceBeard {
                rigid: true,
                ground_level_delta: 0,
                junctions: Vec::new(),
            }),
            refine: None,
        }],
        terrain_adaptation: adjustment,
        pieces_complete: true,
    }
}

/// The Y of the highest [`BlockKind::Stone`] in a shape field's column, or
/// `min_y - 1`.
///
/// Indexes through `OverworldGenerator::shape_index` rather than restating the
/// layout — the field is *not* column-major and a transposed read would report a
/// plausible wrong height at every position.
fn solid_top(generator: &OverworldGenerator, field: &[BlockKind], lx: i32, lz: i32) -> i32 {
    let min_y = generator.min_y();
    for ly in (0..generator.height()).rev() {
        if field[generator.shape_index(lx, ly, lz)] == BlockKind::Stone {
            return min_y + ly;
        }
    }
    min_y - 1
}

/// A beard **raises the ground under the piece**, in the shape field the whole
/// pipeline is built on.
///
/// The piece is placed deliberately *above* the natural surface, which is the
/// situation `beard_thin` exists for: a village house whose floor is above the
/// hillside needs a foundation grown up to meet it. The prediction is therefore
/// directional *and* bounded — the new solid top must reach the piece's floor
/// level, not merely be higher than before.
#[test]
fn a_beard_raises_terrain_under_the_piece() {
    let generator = generator();
    let (cx, cz) = CHUNK;
    let min_y = generator.min_y();
    let height = generator.height();

    let bare = generator.shape_field_with_beard(cx, cz, &Beardifier::empty());
    let natural = solid_top(&generator, &bare, 8, 8);
    assert!(
        natural > min_y && natural < min_y + height - 1,
        "chunk {CHUNK:?} at seed {SEED} has no usable surface (solid top {natural}); \
         pick another chunk rather than relaxing this"
    );

    // A 8×5×8 piece whose floor sits 6 blocks above the natural surface, centred
    // on the chunk. Six is inside the beard kernel's 12-block radius, so a
    // correct beard can reach it; a broken one cannot.
    let floor = natural + 6;
    let box_ = BoundingBox::from_corners(
        [cx * 16 + 4, floor, cz * 16 + 4],
        [cx * 16 + 11, floor + 4, cz * 16 + 11],
    );
    let start = synthetic_start(TerrainAdjustment::BeardThin, box_);
    let beard = Beardifier::for_chunk(cx, cz, [start].iter());
    assert_eq!(beard.rigid_count(), 1, "the synthetic piece must be in reach");

    let bearded = generator.shape_field_with_beard(cx, cz, &beard);
    let raised = solid_top(&generator, &bearded, 8, 8);

    assert!(
        raised > natural,
        "the beard did not raise the ground under the piece: natural {natural}, \
         with beard {raised}"
    );
    // **A prediction, not a direction.** `floor - 1` is the exact answer
    // `Beardifier.java` implies and it is derivable without running anything: the
    // sign of `beard_thin`'s contribution is `-(dy_to_ground + 0.5)`, so at
    // `y == ground_y` the term is *negative* (−0.557 at dx = dz = 0) and at
    // `y == ground_y - 1` it is the mirror positive (+0.557). The foundation
    // therefore stops one block *below* the piece's floor — which is right,
    // because the piece places its own floor block.
    //
    // Both the wrong hypotheses are excluded by this equality: a sign-inverted
    // beard digs down (`raised < natural`, caught above), and a beard whose
    // `ground_level_delta`/`dy` offset is off by one lands on `floor` or
    // `floor - 2`.
    assert_eq!(
        raised,
        floor - 1,
        "the beard should raise the ground to exactly one block below the piece's \
         floor ({}), not {raised}",
        floor - 1
    );
}

/// The beard is **local**. Outside its `affected_box` the shape field is
/// byte-identical, and inside it something changed.
///
/// This is the arm that separates a beard from a uniform density offset: an
/// operand-order slip, a missing `affected_box` test, or a beard accidentally
/// evaluated at the wrong coordinates all produce "terrain changed" and would
/// pass the test above. Failure prints the bounding box of the differing cells,
/// so a wrong *region* is diagnosable without a second run.
#[test]
fn the_beard_is_local_to_its_affected_box() {
    let generator = generator();
    let (cx, cz) = CHUNK;
    let min_y = generator.min_y();
    let height = generator.height();

    let bare = generator.shape_field_with_beard(cx, cz, &Beardifier::empty());
    let natural = solid_top(&generator, &bare, 2, 2);

    // A small piece hugging one corner of the chunk, so its 24-inflated affected
    // box leaves a genuinely untouched region *inside the same chunk*. A piece
    // covering the whole chunk would make the "outside" set empty and the
    // assertion vacuous — that is the trap this geometry avoids.
    let box_ = BoundingBox::from_corners(
        [cx * 16, natural + 2, cz * 16],
        [cx * 16 + 2, natural + 4, cz * 16 + 2],
    );
    let beard = Beardifier::for_chunk(cx, cz, [synthetic_start(TerrainAdjustment::Bury, box_)].iter());
    let affected = beard.affected_box().expect("the piece is in reach");

    let bearded = generator.shape_field_with_beard(cx, cz, &beard);

    let mut outside_diffs = 0usize;
    let mut inside_diffs = 0usize;
    let mut diff_box: Option<([i32; 3], [i32; 3])> = None;
    let mut outside_cells = 0usize;
    for lz in 0..16i32 {
        for lx in 0..16i32 {
            for ly in 0..height {
                let i = generator.shape_index(lx, ly, lz);
                let (wx, wy, wz) = (cx * 16 + lx, min_y + ly, cz * 16 + lz);
                let inside = wx >= affected.min[0]
                    && wx <= affected.max[0]
                    && wy >= affected.min[1]
                    && wy <= affected.max[1]
                    && wz >= affected.min[2]
                    && wz <= affected.max[2];
                if !inside {
                    outside_cells += 1;
                }
                if bare[i] == bearded[i] {
                    continue;
                }
                if inside {
                    inside_diffs += 1;
                } else {
                    outside_diffs += 1;
                }
                diff_box = Some(match diff_box {
                    None => ([wx, wy, wz], [wx, wy, wz]),
                    Some((lo, hi)) => (
                        [lo[0].min(wx), lo[1].min(wy), lo[2].min(wz)],
                        [hi[0].max(wx), hi[1].max(wy), hi[2].max(wz)],
                    ),
                });
            }
        }
    }

    // The control on the control: if the affected box swallowed the chunk there
    // would be nothing outside it, and `outside_diffs == 0` would mean nothing.
    assert!(
        outside_cells > 10_000,
        "only {outside_cells} of this chunk's cells sit outside the affected box — \
         the locality assertion would be near-vacuous; shrink the piece"
    );
    assert!(
        inside_diffs > 0,
        "the beard changed nothing at all inside its own affected box"
    );
    assert_eq!(
        outside_diffs, 0,
        "the beard changed {outside_diffs} cells outside its affected box \
         {affected:?}; all differing cells span {diff_box:?}"
    );
}

/// **The negative control.** A chunk with no adaptation-bearing start in reach
/// gets an empty beardifier, and its shape field is byte-identical to the
/// no-beard arm.
///
/// Byte-identity here is by construction — `fill_stage` branches on
/// `Beardifier::is_empty` and takes the pre-S3 loop verbatim — and this asserts
/// the construction rather than trusting it. Two things are checked, because
/// either alone is passable while the other is broken:
///
/// 1. **`beardifier()` really is empty** for every chunk of a real generated
///    patch, including the chunks that *do* carry structure starts. Every
///    adaptation-bearing kind is on the unsupported ledger today, so a non-empty
///    answer would mean either an incorrectly-ledgered structure or a filter that
///    stopped filtering.
/// 2. **The field is identical** with an explicitly empty beard.
///
/// The workspace-level version of this is `tests/u15_column_dump.rs`, run in two
/// checkouts: that one compares the *wire bytes* across the whole pipeline
/// (surface, carvers, ores, vegetation), which this cannot, because it stops at
/// the shape field.
#[test]
fn no_start_means_no_beard_and_no_change() {
    let generator = generator();
    let mut checked = 0usize;
    for cz in -2..=2 {
        for cx in -2..=2 {
            let real = generator.beardifier(cx, cz);
            assert!(
                real.is_empty(),
                "chunk ({cx}, {cz}) produced a non-empty beardifier ({} rigids, \
                 {} junctions) — no 26.2 structure with a landed piece generator \
                 is adaptation-bearing, so this means a filter regressed",
                real.rigid_count(),
                real.junction_count(),
            );
            let a = generator.shape_field_with_beard(cx, cz, &real);
            let b = generator.shape_field_with_beard(cx, cz, &Beardifier::empty());
            assert!(
                a == b,
                "chunk ({cx}, {cz}) differs between its real (empty) beard and an \
                 explicitly empty one"
            );
            // Non-degeneracy: an all-air field compares equal under any change.
            assert!(
                a.iter().any(|k| *k == BlockKind::Stone),
                "chunk ({cx}, {cz}) generated no stone at all — the comparison above \
                 is vacuous"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 25);
}
