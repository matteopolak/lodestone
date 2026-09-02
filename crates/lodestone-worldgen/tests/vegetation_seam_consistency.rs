//! **Cross-seam continuity for vegetal decoration.** A tree that crosses a chunk
//! border must be served whole — the half in each chunk must belong to the same
//! tree, even though the two chunks compute it independently.
//!
//! # What it is
//!
//! The 3×3 `blockStateWriteRadius(1)` driver
//! ([`lodestone_worldgen::feature::vegetation::apply_vegetal_decoration_step_3x3_per_source`])
//! serves chunk `C` by running all nine of `C ± 1`'s own decoration passes and
//! keeping only what lands in `C`. So the tree standing in chunk `A` and spilling
//! into `B` is computed **twice**: once with `A` as the centre (which supplies the
//! half the player sees in `A`) and once with `B` as the centre (which supplies the
//! half in `B`). Nothing in the engine forces those two computations to agree, and
//! when they disagree the served world keeps one half and drops the other — a tree
//! sliced flat along the chunk border, which is exactly what was reported in-game.
//!
//! Every gate that existed before this one was structurally blind to it.
//! `lodestone_server::chunk::tests::parallel_generation_is_deterministic_and_matches_serial`
//! compares our output against *our own* serial output, so an inconsistency present
//! in both arms is invisible — `decode(encode(x)) == x` in a different costume.
//! `decoration_seam_spill.rs` asserts that a canopy *reaches* across the seam, which
//! it does; it cannot see that the two sides describe different trees.
//!
//! # How it works
//!
//! The expectation comes from **the neighbour's own independent construction**, not
//! from either arm alone: if a drive placed one canopy across the border (tree
//! material at both `x = 15` and `x = 16` of the same `(y, z)` row) then the stitched
//! served field — `x < 16` taken from the drive centred on chunk 0, `x >= 16` from
//! the drive centred on chunk 1, which is literally what a client receives — must
//! also carry tree material at both. A row where it does not is a **truncation**,
//! reported with its side (west half or east half missing) and a bounding box.
//!
//! # The control, and why it is not premise-false
//!
//! [`narrow_read_neighbourhood_is_what_truncates`] runs the *same* fixture in the
//! *same* binary with one variable changed: the sixteen rim chunks of the 5×5 read
//! neighbourhood are supplied as `None`, reproducing the nine-slot read table this
//! file's fix replaced. Nothing else differs — same terrain, same feature data, same
//! driver, same seed. It measured **94** truncated rows against the fixed arm's
//! **44**, and the per-biome split is recorded in [`EXPECTED`] so the control cannot
//! quietly start measuring something else. `minecraft:birch_forest` and
//! `minecraft:old_growth_birch_forest` are the two biomes this landing takes to
//! zero; they are asserted individually rather than only through the total, because a
//! total can be held up by an unrelated biome moving the other way.
//!
//! # The residual, named
//!
//! 44 rows remain, in three biomes, and they are **not** this defect: they come from
//! the second, independent channel — the nine sources write into one shared overlay,
//! so a source's pass sees whichever *other* sources were decorated before it, and
//! both that set and its order are decided by which column is the centre. Isolating
//! each source's writes takes the total to **0** (measured), but it also regresses
//! JVM FULL3X3 parity past `vegetation_parity.rs`'s own measured bound (identity
//! mismatches 1 → 7 at `vegetation_savanna_neg30_15_jvm.txt`, bound 3), because
//! vanilla genuinely does mutate one shared level across the nine passes. Vanilla
//! runs each chunk's features exactly **once** and persists the spill, so it never
//! has to make two computations agree; our recompute-per-centre architecture does,
//! and those two requirements are not simultaneously satisfiable. That is an
//! architectural decision, recorded in `DESIGN.md` §12.118 — not something to close
//! by widening a bound here.
//!
//! # How to change it
//!
//! The floors below are **measurements**. If a later change moves them, re-measure
//! and record the new number with the reason — do not delete the test, which is what
//! happened to this file's predecessor in the same coordinate space (see
//! [`lodestone_worldgen::feature::vegetation::VegGrid`]'s own doc comment).
//!
//! # Dependencies
//!
//! The **bundled production** worldgen data at
//! `crates/lodestone-server/assets/worldgen` (tracked repo state, read straight off
//! disk — no dependency on `lodestone-server` the crate). This crate's own
//! `tests/support/worldgen_data` fixture tree carries only `plains` and `savanna`,
//! and **both of those measure zero truncations at every arm** — a fixture that
//! cannot exercise the defect at all, the "world" species of vacuous test. The
//! biomes that show it are the forest ones.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_worldgen::compose::build_biome_vegetation;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::feature::vegetation::{
    PlacedRef, VegGrid, VegTags, apply_vegetal_decoration_step_3x3_per_source, build_veg_tags,
};
use lodestone_worldgen::feature::region_view::WIDE_RADIUS;
use lodestone_worldgen::feature::{REGION_MAX, REGION_MIN, VEG_PADDING};
use lodestone_worldgen::interner::StateInterner;
use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};
use serde_json::Value;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
/// Flat land at this `y`. Flat on purpose: it makes every biome's tree features
/// actually place, so the fixture *contains* seam-straddling canopies in quantity
/// (asserted below) instead of depending on a lucky coordinate.
const SURFACE: i32 = 63;
const SEED: i64 = 42;

/// The two chunks whose shared border is under test, and the two centres that
/// compute the two halves of any tree crossing it.
const WEST: (i32, i32) = (0, 0);
const EAST: (i32, i32) = (1, 0);

/// Total truncated rows with the 5×5 read neighbourhood in place, summed over every
/// bundled biome. **Measured, and the whole residual is the shared-overlay channel
/// named in the module doc (Cause 2: nine sources share one write overlay, order- and
/// content-dependent, left open on purpose)** — see [`EXPECTED`] for the split.
///
/// **Re-baselined a third time — the biggest single move, and it is a budget moving,
/// not a bug appearing.** Two landings on the same day drove it, and they are
/// mechanically different:
///
/// 1. The mega-jungle/giant-spruce/jungle-bush trunk+foliage placers, then the fancy
///    oak trunk+foliage placer and `FallenTreeFeature`, replaced several
///    `ConfiguredFeature::Unsupported` stubs with real placers. A stub silently
///    consumed zero RNG draws; a real placer draws and mutates the shared per-source
///    overlay, so every feature *downstream of it in the same step* now lands
///    somewhere else than it used to — exactly the mechanism the earlier
///    decoration-step landing exercised before
///    it (see the paragraph below), just with a different set of newly-real feature
///    types. Measured in isolation (`git worktree` at each commit, same fixture, same
///    binary): before these two landings the total was the previous pin, 64; after
///    the mega-tree placers alone it was 400; after the fancy-oak/fallen-tree placer
///    landed on top it settled at **162**, where it stayed through the two
///    unrelated commits between it and cherry/mangrove (a clock-seam sweep, a chunk
///    SPAWN stage) — neither touches vegetation and neither moved the number, which
///    is the expected negative control.
/// 2. Cherry and mangrove trees then landed for real, and `cherry_grove` /
///    `mangrove_swamp` are the *first* bundled biomes whose canopies genuinely
///    straddle this seam — there was nothing to truncate there before because there
///    was nothing placing. That is [`the_fixture_contains_seam_straddling_canopies`]'s
///    own promise working as intended: a biome gaining seam-crossing structure is
///    not evidence of anything wrong with the driver. Measured: 162 → **314** (wide),
///    162's own narrow-arm counterpart 321 → **621** (see
///    [`MEASURED_TOTAL_NARROW`]) — both entirely inside `cherry_grove` (5, 58) and
///    `mangrove_swamp` (18, 71); no other biome's count moved between these two
///    landings.
///
/// **What would legitimately move this number again**: (a) a
/// `ConfiguredFeature::Unsupported`/`PlacementModifier` stub anywhere in the engine
/// starting to place for real — it reshuffles the shared overlay's RNG stream for
/// every biome that runs it in the same `VEGETAL_DECORATION` step, not only the
/// biome the stub belonged to, so a biome with no visible connection to the change
/// moving is expected, not suspicious; (b) a brand-new tree family (like cherry or
/// mangrove here) landing and a previously-silent biome starting to show up in
/// [`EXPECTED`] with a nonzero crossing count. What would **not** be legitimate: a
/// change here with no placer, no feature type and no new tree family in the diff —
/// that has no mechanism to move this number and should be treated as a real
/// regression, not re-pinned.
const MEASURED_TOTAL: usize = 314;

/// Total truncated rows with the read neighbourhood narrowed back to 3×3 — the
/// control. Must exceed [`MEASURED_TOTAL`] by a wide margin, or the widening bought
/// nothing. Re-baselined alongside [`MEASURED_TOTAL`]; see its note for the two
/// landings responsible (162's narrow counterpart was 321; cherry/mangrove then took
/// it to 621).
const MEASURED_TOTAL_NARROW: usize = 621;

/// Per-biome `(west-half-missing, east-half-missing)` at the fixed arm, for every
/// biome that is not zero. Predicted values, not a band: a biome appearing here that
/// should not, or a count moving, is a real change and should fail.
///
/// Nine biomes, up from three. `flower_forest` is untouched by either landing named
/// on [`MEASURED_TOTAL`] and did not move. `bamboo_jungle`, `forest`,
/// `old_growth_pine_taiga` and `old_growth_birch_forest` are new here because a
/// previously-`Unsupported` placer in their `VEGETAL_DECORATION` list now runs (the
/// mega-tree/fancy-oak/fallen-tree landing) — see `old_growth_birch_forest`'s own
/// note below, since it used to be a [`FIXED_TO_ZERO`] guarantee. `jungle` and
/// `old_growth_spruce_taiga` were already here and moved for the same reason.
/// `cherry_grove` and `mangrove_swamp` are new because cherry and mangrove trees are
/// the first real placers either biome has ever had — nothing crossed this seam for
/// them before because nothing placed.
const EXPECTED: &[(&str, usize, usize)] = &[
    ("minecraft:bamboo_jungle", 24, 1),
    ("minecraft:cherry_grove", 5, 58),
    ("minecraft:flower_forest", 7, 8),
    ("minecraft:forest", 7, 0),
    ("minecraft:jungle", 25, 23),
    ("minecraft:mangrove_swamp", 18, 71),
    ("minecraft:old_growth_birch_forest", 14, 0),
    ("minecraft:old_growth_pine_taiga", 4, 24),
    ("minecraft:old_growth_spruce_taiga", 25, 0),
];

/// Biomes this landing takes to exactly zero, asserted individually. Under the
/// narrow control this biome is non-zero, so it is the directional claim: the widened
/// read neighbourhood removed truncation here specifically.
///
/// **`old_growth_birch_forest` left this set** — it is not a regression of the fix
/// this constant is asserting. That fix (the 5×5 read neighbourhood, Cause 1 in the
/// module doc) is unchanged and still holds; what moved is Cause 2 — the shared
/// write-overlay residual documented as open — now measuring nonzero here because
/// the mega-tree/fancy-oak landing gave this biome a placer that previously never
/// drew. It now lives in [`EXPECTED`] at `(14, 0)` instead.
const FIXED_TO_ZERO: &[&str] = &["minecraft:birch_forest"];

fn prod_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")
}

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn try_json(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        std::fs::read_to_string(&path)
            .ok()
            .map(|t| {
                serde_json::from_str(&t).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
            })
            .unwrap_or(Value::Null)
    }
}

impl Resolver for FsResolver {
    /// Never called: this gate seeds its own flat terrain rather than generating
    /// shape, so a call here means the harness stopped doing what it claims.
    fn density_function(&self, id: &str) -> Value {
        panic!("this gate generates no shape; unexpected density_function({id})")
    }
    fn noise(&self, id: &str) -> NoiseParams {
        panic!("this gate generates no shape; unexpected noise({id})")
    }
    fn biome_document(&self, id: &str) -> Value {
        self.try_json("biome", id)
    }
    fn configured_feature(&self, id: &str) -> Value {
        self.try_json("configured_feature", id)
    }
    fn placed_feature(&self, id: &str) -> Value {
        self.try_json("placed_feature", id)
    }
    fn block_tag(&self, id: &str) -> Value {
        self.try_json("tags/block", id)
    }
}

/// One chunk of flat land: stone, three of dirt, grass on top.
fn flat_chunk(interner: &Arc<StateInterner>, cx: i32, cz: i32) -> Arc<DenseBlockGrid> {
    let air = interner.id_of("minecraft:air");
    let mut g = DenseBlockGrid::with_interner(
        Arc::clone(interner),
        cx * 16,
        MIN_Y,
        cz * 16,
        16,
        HEIGHT,
        16,
        air,
    );
    for lx in 0..16 {
        for lz in 0..16 {
            let (x, z) = (cx * 16 + lx, cz * 16 + lz);
            // Only the top few layers matter to decoration (both heightmaps scan
            // down from the sky and stop at the first non-air), so this stops well
            // above `MIN_Y` and the fixture stays cheap.
            for y in SURFACE - 8..SURFACE - 3 {
                g.set(x, y, z, "minecraft:stone");
            }
            for y in SURFACE - 3..SURFACE {
                g.set(x, y, z, "minecraft:dirt");
            }
            g.set(x, SURFACE, z, "minecraft:grass_block[snowy=false]");
        }
    }
    Arc::new(g)
}

/// A flat world spanning the whole 5×5 read neighbourhood of both centres.
fn flat_world(interner: &Arc<StateInterner>) -> HashMap<(i32, i32), Arc<DenseBlockGrid>> {
    let mut world = HashMap::new();
    let lo = WEST.0 - WIDE_RADIUS;
    let hi = EAST.0 + WIDE_RADIUS;
    for cx in lo..=hi {
        for cz in (WEST.1 - WIDE_RADIUS)..=(WEST.1 + WIDE_RADIUS) {
            world.insert((cx, cz), flat_chunk(interner, cx, cz));
        }
    }
    world
}

fn is_tree(s: &str) -> bool {
    s.contains("_leaves") || s.contains("_log") || s.contains("_wood") || s.contains("_stem")
}

/// One full 3×3 drive centred on `centre`, returning every write it made in absolute
/// coordinates.
///
/// `rim` chooses the read neighbourhood: `Rim::Real` is production (the 5×5 this
/// landing introduced), `Rim::Air` narrows it back to the nine-slot table by handing
/// `None` for every offset outside `±1`. That single argument is the control.
fn drive(
    world: &HashMap<(i32, i32), Arc<DenseBlockGrid>>,
    interner: &Arc<StateInterner>,
    centre: (i32, i32),
    features: &[(usize, PlacedRef)],
    tags: &VegTags,
    rim: Rim,
) -> HashMap<(i32, i32, i32), String> {
    let mut grid = VegGrid::with_sources(
        Arc::clone(interner),
        MIN_Y,
        HEIGHT,
        centre.0 * 16,
        centre.1 * 16,
        REGION_MIN - VEG_PADDING,
        REGION_MAX + VEG_PADDING,
        |dx, dz| {
            if rim == Rim::Air && (dx.abs() > 1 || dz.abs() > 1) {
                return None;
            }
            world.get(&(centre.0 + dx, centre.1 + dz)).map(Arc::clone)
        },
    );
    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let for_source = |_x: i32, _z: i32| -> &[(usize, PlacedRef)] { features };
    apply_vegetal_decoration_step_3x3_per_source(
        &mut random,
        SEED,
        centre.0,
        centre.1,
        &mut grid,
        tags,
        &for_source,
    );
    grid.dirty_cells()
        .map(|(x, y, z, s)| ((x, y, z), s.to_string()))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rim {
    /// Production: the sixteen rim chunks carry real terrain.
    Real,
    /// The control: rim reads answer air, as the nine-slot read table did.
    Air,
}

/// What one seam measured.
#[derive(Default)]
struct Seam {
    /// Rows where a drive placed a canopy across the border and the served field
    /// lost the half in the **west** chunk.
    west_missing: usize,
    /// …and the half in the **east** chunk.
    east_missing: usize,
    /// Rows where some drive did place one canopy across the border at all — the
    /// denominator, and the non-vacuity guard.
    crossings: usize,
    /// `(y_min, y_max, z_min, z_max)` of the truncated rows, so a failure says
    /// *where* rather than only *how much*.
    bbox: Option<(i32, i32, i32, i32)>,
}

fn measure_seam(
    west_drive: &HashMap<(i32, i32, i32), String>,
    east_drive: &HashMap<(i32, i32, i32), String>,
) -> Seam {
    let border = EAST.0 * 16;
    // Exactly what the client receives: each chunk's own columns from its own drive.
    let served = |x: i32, y: i32, z: i32| -> bool {
        let m = if x < border { west_drive } else { east_drive };
        m.get(&(x, y, z)).is_some_and(|s| is_tree(s))
    };
    let mut out = Seam::default();
    for d in [west_drive, east_drive] {
        for (&(x, y, z), state) in d.iter() {
            if x != border - 1 || !is_tree(state) {
                continue;
            }
            // This drive says one canopy occupies both sides of the border here.
            if !d.get(&(border, y, z)).is_some_and(|s| is_tree(s)) {
                continue;
            }
            out.crossings += 1;
            let (w, e) = (served(border - 1, y, z), served(border, y, z));
            if !w {
                out.west_missing += 1;
            }
            if !e {
                out.east_missing += 1;
            }
            if !w || !e {
                out.bbox = Some(match out.bbox {
                    None => (y, y, z, z),
                    Some(b) => (b.0.min(y), b.1.max(y), b.2.min(z), b.3.max(z)),
                });
            }
        }
    }
    out
}

/// Every bundled biome that resolves a non-empty `VEGETAL_DECORATION` list, sorted.
fn biomes_with_vegetation(resolver: &FsResolver) -> Vec<String> {
    let dir = prod_dir().join("biome");
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| {
        panic!(
            "cannot read the bundled biome documents at {}: {e} — this gate reads \
             `crates/lodestone-server/assets/worldgen` directly (tracked repo state, \
             not a generated or ignored directory). This crate's own \
             `tests/support/worldgen_data` tree carries only plains and savanna, and \
             neither can exercise the defect at all.",
            dir.display()
        )
    });
    let mut out: Vec<String> = entries
        .map(|e| {
            let name = e.expect("reading a biome document").file_name();
            format!(
                "minecraft:{}",
                name.to_string_lossy().trim_end_matches(".json")
            )
        })
        .filter(|b| !build_biome_vegetation(resolver, b).is_empty())
        .collect();
    out.sort();
    out
}

/// Runs every biome at one seam and returns `(per-biome seam, totals)`.
fn sweep(rim: Rim) -> (Vec<(String, Seam)>, usize, usize) {
    let resolver = FsResolver { root: prod_dir() };
    let tags = build_veg_tags(&resolver);
    let interner = Arc::new(StateInterner::new());
    let world = flat_world(&interner);
    let biomes = biomes_with_vegetation(&resolver);
    assert!(
        biomes.len() >= 40,
        "only {} bundled biomes resolved a vegetation list; the bundled data or the \
         resolver seam has changed and this sweep is no longer covering the tree biomes",
        biomes.len(),
    );
    let mut rows = Vec::new();
    let mut truncated = 0usize;
    let mut crossings = 0usize;
    for biome in biomes {
        let features = build_biome_vegetation(&resolver, &biome);
        let w = drive(&world, &interner, WEST, &features, &tags, rim);
        let e = drive(&world, &interner, EAST, &features, &tags, rim);
        let seam = measure_seam(&w, &e);
        truncated += seam.west_missing + seam.east_missing;
        crossings += seam.crossings;
        rows.push((biome, seam));
    }
    (rows, truncated, crossings)
}

/// The fixture must actually contain the structure this gate exists to judge. A flat
/// plains patch would satisfy every count below while containing no seam-straddling
/// canopy at all, which is unreadable from the assertions themselves — so it is
/// asserted, loudly, first.
#[test]
fn the_fixture_contains_seam_straddling_canopies() {
    let (rows, _, crossings) = sweep(Rim::Real);
    let with_crossings: Vec<&str> = rows
        .iter()
        .filter(|(_, s)| s.crossings > 0)
        .map(|(b, _)| b.as_str())
        .collect();
    assert!(
        crossings >= 300,
        "the fixture carries only {crossings} rows where a canopy crosses the \
         {WEST:?}|{EAST:?} border, across {} biomes — far below the measured 1,000+. \
         Without straddling trees this whole file is vacuous: every truncation count \
         would read zero for a reason unrelated to seam handling.",
        with_crossings.len(),
    );
    assert!(
        with_crossings.len() >= 15,
        "only {} biomes produce a border-crossing canopy ({with_crossings:?}); the \
         sweep is no longer broad enough to be evidence about the engine rather than \
         about one biome",
        with_crossings.len(),
    );
    println!("fixture: {crossings} border-crossing canopy rows across {} biomes", with_crossings.len());
}

/// The claim: with the 5×5 read neighbourhood, a canopy that any drive places across
/// the border survives into the served field, except for the named shared-overlay
/// residual.
#[test]
fn a_canopy_crossing_a_chunk_border_is_served_whole() {
    let (rows, total, _) = sweep(Rim::Real);
    let mut report = Vec::new();
    for (biome, seam) in &rows {
        if seam.west_missing + seam.east_missing == 0 {
            continue;
        }
        report.push(format!(
            "{biome}: west_half_missing={} east_half_missing={} of {} crossings, \
             bbox(y_min,y_max,z_min,z_max)={:?}, border x={}",
            seam.west_missing,
            seam.east_missing,
            seam.crossings,
            seam.bbox,
            EAST.0 * 16,
        ));
    }
    let mut mismatches = Vec::new();
    for &(biome, w, e) in EXPECTED {
        let seam = &rows
            .iter()
            .find(|(b, _)| b == biome)
            .unwrap_or_else(|| panic!("{biome} is no longer in the sweep"))
            .1;
        if (seam.west_missing, seam.east_missing) != (w, e) {
            mismatches.push(format!(
                "{biome}: predicted ({w}, {e}) truncated rows — the named \
                 shared-overlay residual, see the module doc — but measured ({}, \
                 {}). bbox={:?}.",
                seam.west_missing, seam.east_missing, seam.bbox,
            ));
        }
    }
    for &biome in FIXED_TO_ZERO {
        let seam = &rows
            .iter()
            .find(|(b, _)| b == biome)
            .unwrap_or_else(|| panic!("{biome} is no longer in the sweep"))
            .1;
        if (seam.west_missing, seam.east_missing) != (0, 0) {
            mismatches.push(format!(
                "{biome} must serve every border-crossing canopy whole — this is \
                 the biome the 5×5 read neighbourhood was landed for, and the \
                 control measures it non-zero at 3×3. Got ({}, {}) of {} \
                 crossings, bbox={:?}",
                seam.west_missing, seam.east_missing, seam.crossings, seam.bbox,
            ));
        }
    }
    if total != MEASURED_TOTAL {
        mismatches.push(format!(
            "total truncated seam rows moved from the measured {MEASURED_TOTAL} \
             to {total} (the 3×3 control measures {MEASURED_TOTAL_NARROW})",
        ));
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatch(es); re-measure and record the new number with the reason, \
         do not widen blindly:\n{}\n\nfull per-biome report:\n{}",
        mismatches.len(),
        mismatches.join("\n"),
        report.join("\n"),
    );
    println!("truncated seam rows: {total} (3x3 control: {MEASURED_TOTAL_NARROW})\n{}", report.join("\n"));
}

/// The control, and it must be **observed** failing the assertion above rather than
/// described. One variable: the sixteen rim chunks of the read neighbourhood answer
/// air, which is exactly the nine-slot read table
/// [`lodestone_worldgen::feature::region_view::WIDE_RADIUS`] replaced.
///
/// Without this, `a_canopy_crossing_a_chunk_border_is_served_whole` could be passing
/// because nothing in the fixture ever truncates — indistinguishable, from the
/// assertions alone, from a working fix.
#[test]
fn narrow_read_neighbourhood_is_what_truncates() {
    let (rows, narrow_total, narrow_crossings) = sweep(Rim::Air);
    let (_, wide_total, wide_crossings) = sweep(Rim::Real);
    assert!(
        narrow_crossings > 0 && wide_crossings > 0,
        "control premise: both arms must contain border-crossing canopies \
         (narrow={narrow_crossings}, wide={wide_crossings}), or neither measurement \
         is about seams",
    );
    assert_eq!(
        narrow_total, MEASURED_TOTAL_NARROW,
        "the 3×3 read neighbourhood measured {narrow_total} truncated seam rows, not \
         the recorded {MEASURED_TOTAL_NARROW}. This is the control: if it has moved, \
         it is no longer evidence about what the widening fixed.",
    );
    assert!(
        narrow_total > wide_total,
        "control failed to fire: narrowing the read neighbourhood back to 3×3 \
         produced {narrow_total} truncated rows against the widened arm's \
         {wide_total}. The widening must be observed making a difference.",
    );
    for &biome in FIXED_TO_ZERO {
        let seam = &rows.iter().find(|(b, _)| b == biome).expect("biome in sweep").1;
        assert!(
            seam.west_missing + seam.east_missing > 0,
            "control failed for {biome}: it must truncate under the 3×3 read table, \
             or its zero in the widened arm proves nothing about the widening",
        );
    }
    println!("control: 3x3 read neighbourhood -> {narrow_total} truncated rows; 5x5 -> {wide_total}");
}
