//! Gate for "grass/ferns/sunflowers/etc. are sometimes black on one side if
//! there's a block" — a `mesh_models` bug, not a lighting-engine bug.
//!
//! # The defect
//!
//! Vanilla's `ModelBlockRenderer`/`BlockModelLighter` samples a quad's light
//! from **the neighbour the quad's `cullface` opens into** if it has one, or
//! from **the neighbour in `quad.direction()`** only when the quad's plane is
//! flush with the block boundary (`faceCubic`, `BlockModelLighter.java:
//! 265-272`). An unculled quad whose plane is *not* on the boundary — a cross
//! blade, whose `cross.json` element sits on a diagonal — is lit from the
//! **block's own cell** instead.
//!
//! `crates/lodestone-render/src/models.rs`'s `mesh_models` used to sample
//! `pos + quad.direction` unconditionally, for every quad. A short_grass
//! blade next to a solid block therefore read the interior of that solid,
//! which the light engine stores as `0` — the reported "black on one side".
//! The fix adds `quad_is_on_face_boundary` and a `sample_dir` selection that
//! reproduces vanilla's branch (`models.rs`, next to `mesh_models`).
//!
//! # Why mesh-level, not pixel-level
//!
//! The defect is entirely in the light *byte* `mesh_models` writes; the
//! shader's consumption of that byte is already covered by
//! `model_shade_gamma_gate` and `grass_light_response_gate`. No GPU adapter
//! is needed to observe it — see `docs/model-smooth-lighting.md`.
//!
//! # Real baked geometry, not a hand-authored quad
//!
//! The bug lives in the baked `direction`/`cullface`/vertex positions
//! `cross.json`'s `angle: 45` rotation produces (see
//! `lodestone_assets::bake::calculate_facing`), so a hand-authored "cross
//! plant" fixture would be the *world* species of vacuous test — CLAUDE.md's
//! term for a gate whose input data cannot exercise the code path it exists
//! to check. This gate loads the real `minecraft:short_grass` and
//! `minecraft:stone` models from a fetched vanilla `client.jar`, exactly like
//! `grass_light_response_gate.rs`. `#[ignore]`d on `require_client_jar`; run
//! with:
//! `cargo test -p lodestone-render --test cross_plant_light_position_gate -- --ignored --nocapture`
//!
//! # The falsifiable prediction
//!
//! `calculate_facing` snaps a diagonal quad's normal to the nearest axis,
//! tie-broken by `DIRECTIONS`' `Down, Up, North, South, East, West` order. A
//! 45°-about-Y cross rotation puts every quad's normal exactly on a
//! North/South-vs-East/West tie, so all four cross-plant quads bake to
//! `North` or `South`, two each, and an East/West solid neighbour has **no
//! effect at all**. [`the_falsifiable_prediction_all_four_quads_bake_to_north_or_south`]
//! checks this directly; if it fails, the whole diagnosis is wrong.
//!
//! # Ambient occlusion is deliberately disabled in this view
//!
//! `short_grass`'s real model already has `"ambientocclusion": false`
//! (`cross.json`), so the flat, unblended path is what vanilla actually
//! takes for it. For `stone` — whose real model defaults `ambientocclusion`
//! to `true` — this view's [`View::ambient_occlusion_at`] answers `false`
//! anyway, deliberately: `quad_corner_sample`'s AO/light *averaging* math has
//! its own dedicated unit coverage in `models.rs` (`ao_matches_vanillas_…`,
//! `smooth_blend_substitutes_…`), and blending it in here would smear a
//! precise, predicted light byte into a rounded average of several
//! synthetic-view neighbours — turning an exact-value assertion back into
//! the "did it get brighter" shape CLAUDE.md calls out as vacuous. Isolating
//! the *which-cell* bug from the *how-to-average* code keeps every
//! assertion below an exact byte.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use lodestone_assets::{BakedQuad, Direction, ResourceManager, ZipSource};
use lodestone_model::{BlockStateRegistry, Identifier};
use lodestone_render::{BlockModels, ModelSectionView, blocks_json_registry, mesh_models};

#[path = "../gate_harness/mod.rs"]
mod gate_harness;
use gate_harness::{require_blocks_report, require_client_jar};

/// The plant's own cell: full daylight.
const GRASS_OWN: u8 = 0xF0;
/// The plant's north neighbour: a solid block, stored dark by the light
/// engine — this is the value a "sample the neighbour unconditionally" bug
/// reads.
const GRASS_NORTH_NEIGHBOUR: u8 = 0x00;
/// The stone control's own cell: distinguishable from both `0x00` and
/// `0xF0` so assertion 2 cannot pass by accident (see its doc comment).
const STONE_OWN: u8 = 0x0A;
/// The stone control's north neighbour.
const STONE_NORTH_NEIGHBOUR: u8 = 0x00;
/// Every other cell in the view: open air.
const AIR: u8 = 0xF0;

/// The first state id whose block matches `block` and whose properties are a
/// superset of `want`. Mirrors `grass_light_response_gate.rs`.
fn find_state(reg: &dyn BlockStateRegistry, block: &str, want: &[(&str, &str)]) -> u32 {
    let ident: Identifier = block.parse().expect("valid identifier");
    let wanted: BTreeMap<&str, &str> = want.iter().copied().collect();
    (0..reg.state_count())
        .find(|&id| {
            let Some(state) = reg.resolve(id) else {
                return false;
            };
            if *state.block != ident {
                return false;
            }
            wanted
                .iter()
                .all(|(k, v)| state.properties.get(*k).map(String::as_str) == Some(*v))
        })
        .unwrap_or_else(|| panic!("no state found for {block} with properties {want:?}"))
}

fn build_models() -> (BlockModels, Box<dyn BlockStateRegistry>) {
    let jar = require_client_jar();
    let report = require_blocks_report(&jar);
    let source = ZipSource::open(&jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(source)]);
    let registry = blocks_json_registry(&report).expect("parse blocks.json into a registry");
    let models = BlockModels::build(&manager, &registry).expect("bake block models");
    (models, Box::new(registry))
}

/// The block-local bounding box of a quad's four corners — the diagnostic
/// column CLAUDE.md's evidence standards ask for: "failure output must say
/// *where*", not just a percentage or a pass/fail bit.
fn bbox(q: &BakedQuad) -> ([f32; 3], [f32; 3]) {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for p in &q.positions {
        for i in 0..3 {
            lo[i] = lo[i].min(p[i]);
            hi[i] = hi[i].max(p[i]);
        }
    }
    (lo, hi)
}

/// Independent re-derivation of `models.rs`'s private `quad_is_on_face_boundary`,
/// for the diagnostic printout only — this gate must not trust the function
/// under test to describe its own failure.
fn on_face_boundary(q: &BakedQuad) -> bool {
    const EPS: f32 = 1e-4;
    let (fixed, plane) = match q.direction {
        Direction::West => (0, 0.0),
        Direction::East => (0, 1.0),
        Direction::Down => (1, 0.0),
        Direction::Up => (1, 1.0),
        Direction::North => (2, 0.0),
        Direction::South => (2, 1.0),
    };
    q.positions.iter().all(|p| (p[fixed] - plane).abs() <= EPS)
}

fn describe_quad(i: usize, q: &BakedQuad, light: u8) -> String {
    let (lo, hi) = bbox(q);
    let mut s = String::new();
    let _ = write!(
        s,
        "  [{i}] dir={:?} cullface={:?} on_boundary={} bbox=({:.3},{:.3},{:.3})..({:.3},{:.3},{:.3}) light={light:#04x}",
        q.direction,
        q.cullface,
        on_face_boundary(q),
        lo[0], lo[1], lo[2], hi[0], hi[1], hi[2],
    );
    s
}

/// The section view backing every assertion. `(1, 1, 1)` holds
/// `short_grass`'s real baked quads, `(5, 1, 1)` holds `stone`'s. Both have a
/// north (`z - 1`) neighbour distinguishable from their own cell.
///
/// `(5, 1, 0)` — stone's north neighbour — deliberately does **not** occlude
/// (`occludes_at` is `false` there), unlike the plain "stone neighbour"
/// framing might suggest. If it did occlude, `mesh_models`'s existing
/// `cullface`-vs-neighbour-occlusion check (unrelated to this fix, and
/// correct) would cull the very quad assertion 2 needs to inspect — the
/// north face of two abutting opaque cubes is genuinely never drawn in
/// vanilla either. A non-occluding cell that nonetheless *stores* dark light
/// is not contrived: it is exactly the transient state of a torch-lit cell
/// the instant the torch is removed, before relight lands.
struct View {
    grass_quads: Vec<BakedQuad>,
    stone_quads: Vec<BakedQuad>,
}

impl ModelSectionView for View {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        match (x, y, z) {
            (1, 1, 1) => &self.grass_quads,
            (5, 1, 1) => &self.stone_quads,
            _ => &[],
        }
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        // Only stone's own cell occludes — a plant never does. Getting this
        // backwards (occluding the grass cell too) forces `own_is_full_cube`
        // for the plant and silently hides the bug this gate exists to
        // catch: an earlier draft of this file did exactly that and the
        // grass assertion failed with the *broken* histogram even though
        // `mesh_models` was already fixed.
        (x, y, z) == (5, 1, 1)
    }

    /// The honest neighbour rule — `corner_light_at` at the cell the face
    /// opens into. This view encodes no opinion about which cell
    /// `mesh_models` *should* read; that is entirely `sample_dir`'s call.
    fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
        let n = lodestone_render::face_of_direction(dir).normal();
        self.corner_light_at(x as i32 + n[0], y as i32 + n[1], z as i32 + n[2])
    }

    fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
        match (x, y, z) {
            (1, 1, 1) => GRASS_OWN,
            (1, 1, 0) => GRASS_NORTH_NEIGHBOUR,
            (5, 1, 1) => STONE_OWN,
            (5, 1, 0) => STONE_NORTH_NEIGHBOUR,
            _ => AIR,
        }
    }

    /// See the module doc: deliberately flat, to isolate the cell-selection
    /// bug from the (separately, already covered) AO-averaging math.
    fn ambient_occlusion_at(&self, _x: usize, _y: usize, _z: usize) -> bool {
        false
    }
}

fn short_grass_quads(models: &BlockModels, reg: &dyn BlockStateRegistry) -> Vec<BakedQuad> {
    let state = find_state(reg, "minecraft:short_grass", &[]);
    models.quads(state).to_vec()
}

fn stone_quads(models: &BlockModels, reg: &dyn BlockStateRegistry) -> Vec<BakedQuad> {
    let state = find_state(reg, "minecraft:stone", &[]);
    models.quads(state).to_vec()
}

/// The falsifiable prediction from the diagnosis: every one of
/// `short_grass`'s four quads bakes to `North` or `South` — never `East` or
/// `West` — because `cross.json`'s 45°-about-Y rotation puts each quad's
/// normal exactly on a N/S-vs-E/W tie, and `DIRECTIONS` breaks it toward
/// N/S. If this does not hold, the "sometimes black on one side" report
/// cannot be explained by this mechanism and the rest of this gate is
/// measuring the wrong thing.
#[test]
#[ignore = "requires a fetched vanilla client.jar; run explicitly"]
fn the_falsifiable_prediction_all_four_quads_bake_to_north_or_south() {
    let (models, reg) = build_models();
    let grass_quads = short_grass_quads(&models, reg.as_ref());
    assert_eq!(grass_quads.len(), 4, "short_grass must bake to exactly 4 quads (two elements)");

    let north = grass_quads.iter().filter(|q| q.direction == Direction::North).count();
    let south = grass_quads.iter().filter(|q| q.direction == Direction::South).count();
    let east_west = grass_quads
        .iter()
        .filter(|q| matches!(q.direction, Direction::East | Direction::West))
        .count();

    let report: String = grass_quads
        .iter()
        .enumerate()
        .map(|(i, q)| describe_quad(i, q, 0))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        (north, south, east_west),
        (2, 2, 0),
        "diagnosis's tie-break prediction did not hold — got {north} north, {south} south, \
         {east_west} east/west. If this is wrong, re-derive the tie-break; do not trust the \
         rest of this gate.\n{report}"
    );
}

/// Assertion 1 (the subject) + its own anti-vacuity check: every vertex of a
/// real `short_grass` quad must carry the plant's own cell light (`0xF0`),
/// not the north neighbour's (`0x00`).
///
/// **Expected value's origin, outside this crate:**
/// `BlockModelLighter.java:207` says the sample cell is `pos` when
/// `faceCubic` is false; `:268`'s `NORTH` arm needs `minZ == maxZ`, which
/// `cross.json`'s `"angle": 45` rotation makes false; and
/// `ModelBlockRenderer.java:65` plus `"ambientocclusion": false` selects
/// `tesselateFlat`, whose unculled bucket passes `CHECK_LIGHT` (`-1`). The
/// plant's own cell is `0xF0` by this view's construction, so vanilla's
/// answer is `0xF0` for all 16 vertices.
///
/// **The broken build's exact prediction** (not executed here — asserting
/// the *correct* exact histogram already excludes it, which is the point):
/// the two `Direction::North` quads (8 vertices) would read the neighbour
/// `0x00`, the two `Direction::South` quads (8 vertices) would read their
/// own (uninteresting, also-air) neighbour `0xF0` — histogram
/// `{0xF0: 8, 0x00: 8}`. A "did it get brighter" assertion passes on both
/// histograms; asserting the exact one does not.
#[test]
#[ignore = "requires a fetched vanilla client.jar; run explicitly"]
fn short_grass_reads_its_own_cell_not_the_solid_neighbour() {
    let (models, reg) = build_models();
    let grass_quads = short_grass_quads(&models, reg.as_ref());
    assert!(
        grass_quads.iter().all(|q| q.cullface.is_none()),
        "precondition: cross.json quads must carry no cullface — if this fails, short_grass's \
         baked model changed and this gate's premise is stale"
    );

    let view = View { grass_quads: grass_quads.clone(), stone_quads: Vec::new() };
    let mesh = mesh_models(&view);
    assert_eq!(mesh.quad_count(), grass_quads.len(), "every unculled cross quad must be emitted");

    let mut histogram: BTreeMap<u8, usize> = BTreeMap::new();
    for v in &mesh.vertices {
        *histogram.entry(v.light).or_insert(0) += 1;
    }

    let report: String = grass_quads
        .iter()
        .zip(mesh.vertices.chunks_exact(4))
        .enumerate()
        .map(|(i, (q, verts))| describe_quad(i, q, verts[0].light))
        .collect::<Vec<_>>()
        .join("\n");

    let mut expected: BTreeMap<u8, usize> = BTreeMap::new();
    expected.insert(GRASS_OWN, 16);
    assert_eq!(
        histogram, expected,
        "short_grass must be lit from its own cell (0xF0) on every vertex — a lower byte here \
         means mesh_models is (still, or again) sampling the solid north neighbour, which is \
         the reported bug.\n{report}"
    );
}

/// Assertion 2 (the important one): a full-cube `stone` control's `North`
/// quad must read the **neighbour's** light (`0x00`), not its **own** cell's
/// distinguishable light (`0x0A`).
///
/// This is the control that rejects the naive "always sample the own cell"
/// fix, which would pass assertion 1 and re-introduce `fda948f` — a
/// uniformly dark world, the exact regression `SnapshotLight`'s doc
/// (`crates/lodestone-shell/src/mesher.rs:624-638`) was written to prevent.
/// `stone`'s faces carry `cullface`, so this exercises the first row of the
/// rule table (`quad.cullface.or_else(...)` in `mesh_models`) independently
/// of the boundary/`own_is_full_cube` clause assertion 3 exercises.
#[test]
#[ignore = "requires a fetched vanilla client.jar; run explicitly"]
fn stone_still_reads_its_neighbour_not_its_own_cell() {
    let (models, reg) = build_models();
    let grass_quads = short_grass_quads(&models, reg.as_ref());
    let stone_quads = stone_quads(&models, reg.as_ref());
    assert_eq!(stone_quads.len(), 6, "a full cube must bake to exactly 6 quads");
    assert!(
        stone_quads.iter().all(|q| q.cullface == Some(q.direction)),
        "precondition: every stone face must carry its own direction as cullface"
    );

    let view = View { grass_quads: grass_quads.clone(), stone_quads: stone_quads.clone() };
    let mesh = mesh_models(&view);
    assert_eq!(
        mesh.quad_count(),
        grass_quads.len() + stone_quads.len(),
        "no quad should be culled — every neighbour this view names is either the subject \
         cells themselves or open air"
    );

    // Grass's quads are emitted first (mesh_models iterates y, z, x; both
    // blocks share y=1, z=1, and x=1 < x=5).
    let stone_verts = &mesh.vertices[grass_quads.len() * 4..];
    let north_idx = stone_quads
        .iter()
        .position(|q| q.direction == Direction::North)
        .expect("stone must bake a North face");
    let north_verts = &stone_verts[north_idx * 4..north_idx * 4 + 4];

    let report = describe_quad(north_idx, &stone_quads[north_idx], north_verts[0].light);
    assert!(
        north_verts.iter().all(|v| v.light == STONE_NORTH_NEIGHBOUR),
        "stone's North quad must read its neighbour's light (0x00), not its own cell's 0x0A — \
         a build that always samples the own cell passes assertion 1 but fails here.\n{report}"
    );
    assert!(
        north_verts.iter().all(|v| v.light != STONE_OWN),
        "explicit rejection of the naive fix: got the own-cell value 0x0A verbatim.\n{report}"
    );
}

/// Assertion 3 (sensitivity control): clone `short_grass`'s two real `North`
/// quads and snap every vertex's `z` to `0.0` — `minZ == maxZ == 0`, so
/// `quad_is_on_face_boundary` now reads true for them. Placed at the same
/// `(1, 1, 1)` with the same dark north neighbour, they must now read the
/// **neighbour** (`0x00`), unlike the real (diagonal, off-boundary) quads in
/// assertion 1, which read their own cell.
///
/// Without this, assertion 1 would be satisfied by any build that returns
/// `0xF0` unconditionally — including one that ignores the view's light
/// table entirely. This proves the predicate is sensitive to sample
/// *position*, not merely to which value happens to be brighter.
#[test]
#[ignore = "requires a fetched vanilla client.jar; run explicitly"]
fn snapping_the_quad_onto_the_boundary_flips_the_sample_to_the_neighbour() {
    let (models, reg) = build_models();
    let grass_quads = short_grass_quads(&models, reg.as_ref());

    let mut snapped: Vec<BakedQuad> = grass_quads
        .iter()
        .filter(|q| q.direction == Direction::North)
        .cloned()
        .collect();
    assert_eq!(snapped.len(), 2, "expected exactly the two North blade quads");
    for q in &mut snapped {
        for p in &mut q.positions {
            p[2] = 0.0;
        }
    }
    assert!(
        snapped.iter().all(on_face_boundary),
        "test setup bug: snapped quads must satisfy the boundary predicate"
    );

    let view = View { grass_quads: snapped.clone(), stone_quads: Vec::new() };
    let mesh = mesh_models(&view);
    assert_eq!(mesh.quad_count(), snapped.len());

    let report: String = snapped
        .iter()
        .zip(mesh.vertices.chunks_exact(4))
        .enumerate()
        .map(|(i, (q, verts))| describe_quad(i, q, verts[0].light))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        mesh.vertices.iter().all(|v| v.light == GRASS_NORTH_NEIGHBOUR),
        "a quad snapped onto the block boundary must be lit from the neighbour (0x00) — if this \
         still reads 0xF0 (the own cell), the position selection is not actually reading \
         `quad_is_on_face_boundary`, it is defaulting to \"bright\" regardless of geometry.\n{report}"
    );
}
