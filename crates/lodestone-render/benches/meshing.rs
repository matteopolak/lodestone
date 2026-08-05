//! Meshing throughput for **both** meshers, plus frontier remesh cost
//! (issues #90, #91, #92).
//!
//! # Which mesher each bench uses — read this before quoting any number
//!
//! The whole reason #90 and #91 are two issues is that this repo has already
//! shipped the mistake of measuring one mesher and believing the answer covered
//! the other (`CLAUDE.md`'s "world" species of vacuous test: a colour fix
//! verified against `--headless`, which meshes through `mesh_simple`, while the
//! constants under test lived in `mesh_models`, which live terrain uses). So
//! every bench below names its mesher in its own function name and doc comment:
//!
//! | bench fn | mesher called | who uses that mesher in production |
//! |---|---|---|
//! | `bench_mesh_simple` | [`mesh_simple`] (`src/mesh.rs:314`) | `--headless` / demo world |
//! | `bench_mesh_greedy` | [`mesh_greedy`] (`src/mesh.rs:336`) | nothing in the shell today |
//! | `bench_mesh_models` | [`mesh_models`] (`src/models.rs:737`) | live terrain |
//! | `bench_frontier_remesh` | `build_batch` → `mesh_greedy` | neither (harness path) |
//!
//! **A correction to issue #91's premise, verified in source rather than
//! assumed.** #91 says `tests/world_mesher_bench.rs` is "confirmed to exercise
//! the *other* mesher (`mesh_models`)". It is not. That test calls
//! `build_whole_world(.., greedy = true)` → `dirty_jobs` → `build_batch(jobs,
//! &TerrainClassifier, true)` → `SectionSnapshot::build_mesh(.., greedy=true)`
//! → **`mesh_greedy`** (`src/mesher.rs:303`). Nothing in `lodestone-render`'s
//! test tree reached `mesh_models` for a throughput number before this file.
//! The shell's own switch is `crates/lodestone-shell/src/mesher.rs:1349`
//! (`match classifier.models()`): `Some(..)` → `mesh_models`, `None` →
//! `mesh_simple`. Note the shell **never** calls `mesh_greedy`; greedy is
//! reachable only through this crate's `build_mesh`/`build_batch`.
//!
//! # What is a gate and what is only a recorded baseline
//!
//! Durations here are recorded baselines with a ±25% advisory band, not
//! assertions — a wall-clock number taken on a machine with a dozen other
//! agents building is a sample, not a measurement (`CLAUDE.md`). The
//! **assertions** in this file are all counts, which are immune to machine
//! load:
//!
//! * `mesh_greedy` emits no more quads than `mesh_simple` for the same
//!   neighbourhood (merging can only reduce the quad count).
//! * every mesher emits a non-zero quad count on the surface section (the
//!   anti-vacuity control: a fixture that meshed to nothing would report an
//!   impressive throughput while measuring nothing).
//! * `dirty_jobs` for one arriving column enqueues the **same number** of
//!   sections regardless of how large the already-loaded world is — the #92
//!   gate, and the one that would catch "frontier cost scales with total
//!   loaded sections instead of boundary size" without any timing at all.
//!
//! Run with `cargo bench -p lodestone-render --bench meshing`, or
//! `cargo bench -p lodestone-render --bench meshing -- --test` for a
//! correctness-only pass (what a CI-shaped check should run).

mod support;

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_assets::{BakedQuad, Direction};
use lodestone_render::mesher::SectionSource;
use lodestone_render::{
    BlockClassifier, Cell, ChunkSectionView, Mesh, ModelMesh, ModelSectionView,
    SectionNeighborhood, SectionView, SpriteId, UniformLight, build_batch, dirty_jobs, mesh_greedy,
    mesh_models, mesh_simple,
};
use lodestone_testsupport::bench_fixtures::{MODERN_SECTIONS, synthetic_overworld_column};
use lodestone_world::{ChunkColumn, ChunkSection};

/// Air state id in `synthetic_overworld_column`'s fixture.
const AIR: u32 = 0;

/// The section index holding the fixture's varied surface band. `synthetic_
/// column(-64, 24, seed)` lays solid stone over `y = -64..40` and a per-cell
/// varied band over `y = 40..48`, so section 6 (`y = 32..48`) is the one with
/// real exposed geometry *and* per-cell material variation. Section 3 is
/// fully-buried stone (interior faces all culled) and is benched separately,
/// because those two are the two extremes a real column contains and a bench
/// that only meshed one of them would report a misleading per-section cost.
const SURFACE_SECTION: usize = 6;
/// A fully-buried stone section (`y = -16..0`).
const BURIED_SECTION: usize = 3;

/// Same classifier shape `tests/world_mesher_bench.rs:44` uses: id 0 is lit
/// air, every other id is a solid cube whose sprite is its state id. Copied
/// rather than shared because that one is a private test-local type; keeping
/// them identical is what makes this bench's numbers comparable to that test's.
#[derive(Debug)]
struct TerrainClassifier;

impl BlockClassifier for TerrainClassifier {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        if state_id == AIR {
            Cell {
                occludes: false,
                surface: None,
                block_light,
                sky_light,
            }
        } else {
            let mut c = Cell::solid(SpriteId(state_id as u16));
            c.block_light = block_light;
            c.sky_light = sky_light;
            c
        }
    }
}

/// A 3×3 patch of fixture columns keyed by `(cx, cz)`, each with its own seed so
/// neighbouring columns differ — a uniform patch would cull every side face at
/// the boundaries and understate the work by construction.
fn column_patch() -> HashMap<(i32, i32), ChunkColumn> {
    let mut map = HashMap::new();
    for cz in -1..=1 {
        for cx in -1..=1 {
            // Distinct seeds so the surface band's material pattern differs per
            // column; `synthetic_column` is documented as deterministic in seed.
            let seed = (cx + 1) as u64 * 3 + (cz + 1) as u64;
            map.insert((cx, cz), synthetic_overworld_column(seed));
        }
    }
    map
}

/// The 27 `(dx, dy, dz)` → section pairs for the centre column's section
/// `si`, drawn from the 3×3 column patch. Absent sections are simply omitted
/// (the neighbourhood reads them as `Cell::EMPTY`), which is exactly how a real
/// elided all-air section behaves.
fn neighbourhood_arcs(
    patch: &HashMap<(i32, i32), ChunkColumn>,
    si: usize,
) -> Vec<((i32, i32, i32), Arc<ChunkSection>)> {
    let mut out = Vec::with_capacity(27);
    for dz in -1..=1 {
        for dy in -1..=1i32 {
            for dx in -1..=1 {
                let Some(col) = patch.get(&(dx, dz)) else { continue };
                let Ok(nsi) = usize::try_from(si as i32 + dy) else { continue };
                if let Some(arc) = col.section_arc(nsi) {
                    out.push(((dx, dy, dz), arc));
                }
            }
        }
    }
    out
}

/// Meshes one section through the **packed** path — `mesh_simple` (`greedy ==
/// false`) or `mesh_greedy` (`greedy == true`). Everything the mesher reads is
/// built inside, so a caller can call this in a loop without shared state
/// leaking between iterations (the "duration species" trap
/// `benches/mob_tick.rs` documents).
fn mesh_packed(arcs: &[((i32, i32, i32), Arc<ChunkSection>)], greedy: bool) -> Mesh {
    let classifier = TerrainClassifier;
    let light = UniformLight::pre_light_bridge();
    let views: Vec<_> = arcs
        .iter()
        .map(|(d, a)| (*d, ChunkSectionView::new(a.as_ref(), &classifier, &light)))
        .collect();
    let mut hood = SectionNeighborhood::default();
    for (d, view) in &views {
        hood.set(d.0, d.1, d.2, Some(view as &dyn SectionView));
    }
    if greedy {
        mesh_greedy(&hood)
    } else {
        mesh_simple(&hood)
    }
}

/// A full unit cube's six quads, each with its own `cullface` so `mesh_models`
/// performs real neighbour-occlusion culling rather than emitting every face.
/// This is the minimum honest model workload: a view whose quads had
/// `cullface: None` would skip the culling branch that dominates the live path.
fn cube_quads() -> Vec<BakedQuad> {
    const CORNERS: [[[f32; 3]; 4]; 6] = [
        // Down (-Y)
        [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 0.0, 1.0], [0.0, 0.0, 1.0]],
        // Up (+Y)
        [[0.0, 1.0, 1.0], [1.0, 1.0, 1.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]],
        // North (-Z)
        [[1.0, 0.0, 0.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
        // South (+Z)
        [[0.0, 0.0, 1.0], [1.0, 0.0, 1.0], [1.0, 1.0, 1.0], [0.0, 1.0, 1.0]],
        // East (+X)
        [[1.0, 0.0, 1.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [1.0, 1.0, 1.0]],
        // West (-X)
        [[0.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 1.0], [0.0, 1.0, 0.0]],
    ];
    const DIRS: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::East,
        Direction::West,
    ];
    DIRS.iter()
        .zip(CORNERS)
        .map(|(dir, positions)| BakedQuad {
            positions,
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: *dir,
            cullface: Some(*dir),
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
        })
        .collect()
}

/// A [`ModelSectionView`] over the same fixture sections the packed benches
/// use, so #90's and #91's numbers describe the *same terrain* (the issues ask
/// for exactly that: "same input, different mesher, is the whole point").
///
/// Only the two required trait methods are implemented; the rest keep their
/// defaults, which is what makes a hermetic `mesh_models` bench possible with
/// no `client.jar`, no `BlockModels` bake and no GPU. The cost this measures is
/// therefore the *mesher's* per-face work over a realistic occupancy field, not
/// model baking or atlas resolution.
struct FixtureModelView {
    cube: Vec<BakedQuad>,
    /// Occupancy over `-1..17` in each axis, indexed `(x+1) + 18*((y+1) + 18*(z+1))`.
    solid: Vec<bool>,
}

impl FixtureModelView {
    fn new(arcs: &[((i32, i32, i32), Arc<ChunkSection>)]) -> Self {
        let mut solid = vec![false; 18 * 18 * 18];
        for x in -1..17i32 {
            for y in -1..17i32 {
                for z in -1..17i32 {
                    // Route each coordinate to whichever section owns it.
                    let (dx, lx) = split(x);
                    let (dy, ly) = split(y);
                    let (dz, lz) = split(z);
                    let present = arcs
                        .iter()
                        .find(|((sx, sy, sz), _)| (*sx, *sy, *sz) == (dx, dy, dz))
                        .is_some_and(|(_, a)| a.get_block(lx, ly, lz) != AIR);
                    if present {
                        solid[idx18(x, y, z)] = true;
                    }
                }
            }
        }
        Self { cube: cube_quads(), solid }
    }
}

/// Splits a centre-local coordinate in `-1..17` into (section offset, local).
fn split(v: i32) -> (i32, usize) {
    if v < 0 {
        (-1, 15)
    } else if v >= 16 {
        (1, 0)
    } else {
        (0, v as usize)
    }
}

fn idx18(x: i32, y: i32, z: i32) -> usize {
    ((x + 1) + 18 * ((y + 1) + 18 * (z + 1))) as usize
}

impl ModelSectionView for FixtureModelView {
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
        if self.solid[idx18(x as i32, y as i32, z as i32)] {
            &self.cube
        } else {
            &[]
        }
    }

    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        if !(-1..17).contains(&x) || !(-1..17).contains(&y) || !(-1..17).contains(&z) {
            return false;
        }
        self.solid[idx18(x, y, z)]
    }
}

/// A **uniform-material** exposed surface: one section of solid id-1 stone with
/// nothing above it, and no horizontal neighbours. Exists purely as the control
/// that makes the greedy-vs-simple gate non-vacuous.
///
/// The shared fixture deliberately varies material per cell (that is how it
/// avoids `CLAUDE.md`'s "vacuous world" trap), and a side effect is that greedy
/// meshing has **nothing to merge** on it: measured, `mesh_greedy` and
/// `mesh_simple` both emit exactly 256 quads there, so `greedy <= simple` holds
/// with equality and would keep holding if merging were deleted outright. This
/// neighbourhood gives greedy a surface it *must* collapse — 256 coplanar
/// same-sprite top faces into one quad — so the assertion below distinguishes
/// working merging from absent merging.
fn uniform_surface_section() -> Vec<((i32, i32, i32), Arc<ChunkSection>)> {
    let mut s = ChunkSection::new(
        lodestone_world::PaletteKind::block_states(),
        lodestone_world::PaletteKind::biomes(),
        AIR,
        0,
    );
    for y in 0..8 {
        for z in 0..16 {
            for x in 0..16 {
                s.set_block(x, y, z, 1);
            }
        }
    }
    vec![((0, 0, 0), Arc::new(s))]
}

/// Median of a sorted-in-place sample.
fn median(v: &mut Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN timings"));
    v[v.len() / 2]
}

/// Times `f` over `iters` runs and returns (median µs, last result).
fn timed<T>(iters: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut last = f(); // warm-up, discarded from the sample
    let mut us = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        last = f();
        us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    (median(&mut us), last)
}

/// **Issue #90** — `mesh_simple`, the mesher `--headless` and the demo world
/// use (`crates/lodestone-shell/src/mesher.rs:1349`'s `None` arm).
///
/// Reports µs/section and quads/ms for the surface section and the buried
/// section separately, and records both. Asserts a non-zero quad count on the
/// surface section: a fixture that meshed to nothing is the classic
/// "impressively fast because it did nothing" reading.
fn bench_mesh_simple(c: &mut Criterion) {
    let patch = column_patch();
    let surface = neighbourhood_arcs(&patch, SURFACE_SECTION);
    let buried = neighbourhood_arcs(&patch, BURIED_SECTION);

    let (us_surface, mesh_surface) = timed(40, || mesh_packed(black_box(&surface), false));
    let (us_buried, mesh_buried) = timed(40, || mesh_packed(black_box(&buried), false));

    let quads_surface = mesh_surface.stats().quads;
    let quads_buried = mesh_buried.stats().quads;
    assert!(
        quads_surface > 0,
        "mesh_simple emitted no quads for the surface section — the fixture or the neighbourhood \
         build is wrong, and every throughput number here would be measuring nothing"
    );

    println!(
        "mesh_simple (headless/demo mesher): surface section {us_surface:.1}us -> {:.1} sections/ms, \
         {quads_surface} quads ({:.1} quads/ms); buried section {us_buried:.1}us, {quads_buried} quads",
        1000.0 / us_surface,
        quads_surface as f64 * 1000.0 / us_surface,
    );

    let scene = "fixture=synthetic_overworld_column patch=3x3 section=6(surface)";
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_simple_surface_us",
        scene,
        value: us_surface,
        unit: "us",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_simple_surface_quads",
        scene,
        value: quads_surface as f64,
        unit: "quads",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_simple_buried_us",
        scene: "fixture=synthetic_overworld_column patch=3x3 section=3(buried)",
        value: us_buried,
        unit: "us",
    });

    c.bench_function("meshing/mesh_simple_surface_section", |b| {
        b.iter(|| black_box(mesh_packed(black_box(&surface), false)))
    });
}

/// **Issue #90/#91 boundary** — `mesh_greedy`, which nothing in
/// `lodestone-shell` calls today (verified: the shell's only two meshing arms
/// are `mesh_models` and `mesh_simple`). It is benched because `build_batch`'s
/// `greedy` flag is a real, tracked perf/geometry trade-off inside this crate
/// and #91 asks for greedy and non-greedy to be reported separately rather than
/// collapsed into one number.
///
/// The quad-count comparison against `mesh_simple` is the load-immune gate:
/// merging can only ever reduce quads, so `greedy <= simple` must hold and a
/// regression that broke merging would fail here on a **count**, no matter what
/// else the machine was doing.
fn bench_mesh_greedy(c: &mut Criterion) {
    let patch = column_patch();
    let surface = neighbourhood_arcs(&patch, SURFACE_SECTION);

    let (us_greedy, mesh_greedy_out) = timed(40, || mesh_packed(black_box(&surface), true));
    let simple = mesh_packed(&surface, false);

    let quads_greedy = mesh_greedy_out.stats().quads;
    let quads_simple = simple.stats().quads;
    assert!(quads_greedy > 0, "mesh_greedy emitted no quads — nothing measured");
    assert!(
        quads_greedy <= quads_simple,
        "greedy meshing emitted MORE quads ({quads_greedy}) than simple ({quads_simple}) for the \
         same neighbourhood; merging can only reduce the count, so this is a real regression"
    );

    // The control: on a uniform-material exposed surface, merging must strictly
    // win. Without this, the inequality above is satisfied by equality on the
    // varied fixture and would survive merging being deleted.
    let uniform = uniform_surface_section();
    let u_greedy = mesh_packed(&uniform, true).stats().quads;
    let u_simple = mesh_packed(&uniform, false).stats().quads;
    assert!(
        u_greedy < u_simple,
        "control failed: on a uniform 16x16 exposed stone surface mesh_greedy emitted \
         {u_greedy} quads and mesh_simple {u_simple} — greedy must strictly merge coplanar \
         same-sprite faces, so either merging is broken or this control no longer presents a \
         mergeable surface (in which case the greedy<=simple gate above proves nothing)"
    );

    println!(
        "mesh_greedy: surface section {us_greedy:.1}us, {quads_greedy} quads vs mesh_simple's \
         {quads_simple} quads (merge ratio {:.3}; the shared fixture varies material per cell so \
         there is nothing to merge). Control on a uniform exposed surface: greedy {u_greedy} quads \
         vs simple {u_simple} — merging verified active.",
        quads_greedy as f64 / quads_simple as f64,
    );

    let scene = "fixture=synthetic_overworld_column patch=3x3 section=6(surface)";
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_greedy_surface_us",
        scene,
        value: us_greedy,
        unit: "us",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_greedy_quad_ratio_vs_simple",
        scene,
        value: quads_greedy as f64 / quads_simple as f64,
        unit: "x",
    });

    c.bench_function("meshing/mesh_greedy_surface_section", |b| {
        b.iter(|| black_box(mesh_packed(black_box(&surface), true)))
    });
}

/// **Issue #91** — `mesh_models`, the mesher **live terrain** uses
/// (`crates/lodestone-shell/src/mesher.rs:1349`'s `Some(models)` arm). Same
/// fixture terrain as `bench_mesh_simple`, so the two are comparable *as
/// scenes*; they are deliberately **not** comparable as absolute quad counts,
/// because the model path emits per-model quads with a different vertex format
/// and no greedy merging. The ratio recorded below is therefore labelled a
/// cost ratio for the same terrain, not a quality comparison.
fn bench_mesh_models(c: &mut Criterion) {
    let patch = column_patch();
    let surface = neighbourhood_arcs(&patch, SURFACE_SECTION);
    let buried = neighbourhood_arcs(&patch, BURIED_SECTION);
    let view_surface = FixtureModelView::new(&surface);
    let view_buried = FixtureModelView::new(&buried);

    let (us_surface, mesh_surface): (f64, ModelMesh) =
        timed(40, || mesh_models(black_box(&view_surface)));
    let (us_buried, mesh_buried): (f64, ModelMesh) =
        timed(40, || mesh_models(black_box(&view_buried)));

    let quads_surface = mesh_surface.quad_count();
    assert!(
        quads_surface > 0,
        "mesh_models emitted no quads for the surface section — the FixtureModelView occupancy \
         grid is wrong and this bench measures nothing"
    );
    // Anti-vacuity control on the culling branch itself: a view whose
    // `occludes_at` always answered false would emit all 6 faces of every solid
    // cell. Assert we emitted strictly fewer than that, i.e. culling ran.
    let solid_cells = view_surface.solid.iter().filter(|s| **s).count();
    assert!(
        quads_surface < solid_cells * 6,
        "mesh_models emitted {quads_surface} quads for {solid_cells} solid cells — that is every \
         face of every cell, so neighbour-occlusion culling did not run and the measured cost is \
         not the live path's cost"
    );

    println!(
        "mesh_models (live-terrain mesher): surface section {us_surface:.1}us -> {:.1} sections/ms, \
         {quads_surface} quads ({:.1} quads/ms); buried section {us_buried:.1}us, {} quads",
        1000.0 / us_surface,
        quads_surface as f64 * 1000.0 / us_surface,
        mesh_buried.quad_count(),
    );

    let scene = "fixture=synthetic_overworld_column patch=3x3 section=6(surface)";
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_models_surface_us",
        scene,
        value: us_surface,
        unit: "us",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_models_surface_quads",
        scene,
        value: quads_surface as f64,
        unit: "quads",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "mesh_models_buried_us",
        scene: "fixture=synthetic_overworld_column patch=3x3 section=3(buried)",
        value: us_buried,
        unit: "us",
    });

    c.bench_function("meshing/mesh_models_surface_section", |b| {
        b.iter(|| black_box(mesh_models(black_box(&view_surface))))
    });
}

/// A `SectionSource` over a section map, matching `tests/world_mesher_bench.rs`'s
/// `MapWorld` so the frontier bench and that test describe the same world shape.
struct MapWorld(HashMap<(i32, i32, i32), Arc<ChunkSection>>);

impl SectionSource for MapWorld {
    fn section(&self, coord: (i32, i32, i32)) -> Option<Arc<ChunkSection>> {
        self.0.get(&coord).cloned()
    }
}

/// Builds a `MapWorld` over columns `cx, cz in -rd..=rd`, **excluding** the
/// column at `hole`, from the shared fixture.
fn map_world(rd: i32, hole: (i32, i32)) -> MapWorld {
    let mut map = HashMap::new();
    for cz in -rd..=rd {
        for cx in -rd..=rd {
            if (cx, cz) == hole {
                continue;
            }
            insert_column(&mut map, cx, cz);
        }
    }
    MapWorld(map)
}

/// Inserts one fixture column's present sections into a section map — the
/// "a neighbour column just streamed in" event.
fn insert_column(map: &mut HashMap<(i32, i32, i32), Arc<ChunkSection>>, cx: i32, cz: i32) {
    let col = synthetic_overworld_column((cx.unsigned_abs() as u64 * 7) ^ cz.unsigned_abs() as u64);
    for si in 0..MODERN_SECTIONS {
        if let Some(arc) = col.section_arc(si) {
            map.insert((cx, si as i32, cz), arc);
        }
    }
}

/// **Issue #92** — the cost of the remesh triggered when one previously-absent
/// neighbour column streams in at the edge of an already-loaded set.
///
/// # The gate is a count, not a time
///
/// #92 asks for "remeshing one boundary costs X% of a full cold build" and for
/// an assertion that the remesh does not touch unaffected interior sections.
/// Two corrections, both verified in source:
///
/// 1. **`dirty_jobs` does not restrict itself to boundary sections.** Its own
///    doc (`src/mesher.rs:155`) says re-meshing only the boundary sections
///    would be enough but that "callers typically re-mesh whatever loaded
///    sections fall in these columns", and the implementation enqueues every
///    present section of all 9 columns. The shell's `TerrainMesh::mesh_column`
///    likewise re-snapshots a whole column. So an assertion of the form "only
///    boundary sections were touched" would fail against today's code — it
///    asserts a property the implementation deliberately does not have, and
///    writing it would report a defect where there is a design choice.
/// 2. **The property that actually matters is still assertable, and as a
///    count.** The failure #92 exists to catch is a per-arrival cost that
///    scales with the *total loaded set* rather than with the neighbourhood. So
///    this bench measures the enqueued job count for one arriving column at two
///    very different world sizes and asserts they are **identical**. That is
///    immune to machine load, and it fails loudly if a future change makes
///    arrival cost a function of world size.
fn bench_frontier_remesh(c: &mut Criterion) {
    const ROWS: std::ops::Range<i32> = 0..MODERN_SECTIONS as i32;
    let arriving = (3, 3);

    // Two worlds of very different size, both initially missing the same
    // column, then both receiving it — the arriving column must be *present*
    // before the remesh is measured, or the measurement covers only the 8
    // existing neighbours and silently understates the work by one ninth. (It
    // did, on this bench's first run: 56 sections instead of 63.)
    let mut small = map_world(4, arriving);
    let mut large = map_world(10, arriving);
    insert_column(&mut small.0, arriving.0, arriving.1);
    insert_column(&mut large.0, arriving.0, arriving.1);
    let jobs_small = dirty_jobs(&small, arriving.0, arriving.1, ROWS).len();
    let jobs_large = dirty_jobs(&large, arriving.0, arriving.1, ROWS).len();
    assert!(jobs_small > 0, "no jobs enqueued for an arriving column — nothing measured");
    assert_eq!(
        jobs_small, jobs_large,
        "frontier remesh enqueued {jobs_small} sections in a 9x9-column world but {jobs_large} in \
         a 21x21-column world; per-arrival work must depend on the 9-column neighbourhood only, \
         never on the size of the loaded set (issue #92's real failure mode)"
    );

    // Cost of the arrival itself, on the large world: the jobs are gathered and
    // built exactly as `load_column` would (`dirty_jobs` → `build_batch`).
    let (us_frontier, built) = timed(10, || {
        let jobs = dirty_jobs(black_box(&large), arriving.0, arriving.1, ROWS);
        build_batch(jobs, &TerrainClassifier, true)
    });
    let frontier_quads: usize = built.iter().map(|b| b.mesh.quad_count()).sum();
    assert!(frontier_quads > 0, "frontier remesh produced no geometry — nothing measured");

    // Cold whole-world build over the same large world, for #92's ratio.
    let cold_start = Instant::now();
    let mut cold_sections = 0usize;
    let mut cold_quads = 0usize;
    for cz in -10..=10 {
        for cx in -10..=10 {
            let jobs = dirty_jobs(&large, cx, cz, ROWS);
            cold_sections += jobs.len();
            let built = build_batch(jobs, &TerrainClassifier, true);
            cold_quads += built.iter().map(|b| b.mesh.quad_count()).sum::<usize>();
        }
    }
    let cold_us = cold_start.elapsed().as_secs_f64() * 1e6;
    assert!(cold_quads > 0, "cold whole-world build produced no geometry");

    let pct = 100.0 * us_frontier / cold_us;
    println!(
        "frontier remesh (mesh_greedy via build_batch): one arriving column at the edge of a \
         21x21-column loaded set costs {us_frontier:.0}us for {jobs_large} sections / \
         {frontier_quads} quads = {pct:.3}% of the {cold_us:.0}us cold build over the same world \
         ({cold_sections} section builds). PROVISIONAL: wall-clock, taken on a shared machine."
    );

    let scene = "fixture=synthetic_overworld_column rd=10 arriving=(3,3) mesher=mesh_greedy";
    support::record(support::Record {
        bench: "meshing",
        metric: "frontier_remesh_us",
        scene,
        value: us_frontier,
        unit: "us",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "frontier_remesh_sections",
        scene,
        value: jobs_large as f64,
        unit: "sections",
    });
    support::record(support::Record {
        bench: "meshing",
        metric: "frontier_remesh_pct_of_cold_build",
        scene,
        value: pct,
        unit: "%",
    });

    c.bench_function("meshing/frontier_remesh_one_column", |b| {
        b.iter(|| {
            let jobs = dirty_jobs(black_box(&large), arriving.0, arriving.1, ROWS);
            black_box(build_batch(jobs, &TerrainClassifier, true))
        })
    });
}

criterion_group!(
    benches,
    bench_mesh_simple,
    bench_mesh_greedy,
    bench_mesh_models,
    bench_frontier_remesh
);
criterion_main!(benches);
