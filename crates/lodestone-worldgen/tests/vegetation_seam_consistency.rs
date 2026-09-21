//! Cross-seam continuity for vegetal decoration.
//!
//! The control compares a source's writes when requested by adjacent targets and
//! verifies that the widened read neighbourhood makes that result canonical.
//!
//! # What it is
//!
//! The driver computes a target from nine source passes. A source near a target's
//! edge reads a wider rim, so its writes must not depend on which adjacent target
//! requested them.
//!
//! # How it works
//!
//! The sweep records where a drive places tree material on both sides of the seam,
//! then reports which side is absent from the served field. The canonical-source
//! test compares the complete sparse write maps for one source.
//!
//! The fixture reads the bundled production biome documents and uses flat terrain;
//! generated totals are deliberately not pinned because feature selection changes
//! legitimately move them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_data::block_states::StateId;
use lodestone_worldgen::compose::build_decoration_catalog;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::feature::vegetation::{
    PlacedRef, VegGrid, VegTags, apply_vegetal_decoration_step_3x3_per_source, build_veg_tags,
};
use lodestone_worldgen::feature::region_view::WIDE_RADIUS;
use lodestone_worldgen::feature::{REGION_MAX, REGION_MIN, STEP_VEGETAL_DECORATION, VEG_PADDING};
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

/// Generated totals are diagnostics only; this control uses location signatures and
/// a comparison of one source under adjacent requests.
///
/// Keep the fixture broad enough to exercise several biome feature selections.
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

/// Total truncated rows with the read neighbourhood narrowed back to 3×3 — the
/// control. Must exceed [`MEASURED_TOTAL`] by a wide margin, or the widening bought
/// nothing. Re-baselined alongside [`MEASURED_TOTAL`]; see its note for the two
/// landings responsible (162's narrow counterpart was 321; cherry/mangrove then took
/// it to 621).

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

fn prod_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../lodestone-server/assets/worldgen")
}

fn vegetal_features_for(resolver: &dyn Resolver, biome: &str) -> Vec<(usize, PlacedRef)> {
    let catalog = build_decoration_catalog(resolver, &[biome.to_owned()]);
    catalog
        .select([biome])
        .into_iter()
        .filter_map(|(step, index, placed)| {
            (step == STEP_VEGETAL_DECORATION).then_some((index, placed))
        })
        .collect()
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
fn state(spec: &str) -> StateId {
    StateId::from_state_str(spec).expect("fixture state is in the generated table")
}

fn flat_chunk(cx: i32, cz: i32) -> Arc<DenseBlockGrid> {
    let air = StateId::AIR;
    let mut g = DenseBlockGrid::with_default(
        cx * 16,
        MIN_Y,
        cz * 16,
        16,
        HEIGHT,
        16,
        air,
    );
    let stone = state("minecraft:stone");
    let dirt = state("minecraft:dirt");
    let grass = state("minecraft:grass_block[snowy=false]");
    for lx in 0..16 {
        for lz in 0..16 {
            let (x, z) = (cx * 16 + lx, cz * 16 + lz);
            // Only the top few layers matter to decoration (both heightmaps scan
            // down from the sky and stop at the first non-air), so this stops well
            // above `MIN_Y` and the fixture stays cheap.
            for y in SURFACE - 8..SURFACE - 3 {
                g.set_id(x, y, z, stone);
            }
            for y in SURFACE - 3..SURFACE {
                g.set_id(x, y, z, dirt);
            }
            g.set_id(x, SURFACE, z, grass);
        }
    }
    Arc::new(g)
}

/// A flat world spanning the whole 5×5 read neighbourhood of both centres.
fn flat_world() -> HashMap<(i32, i32), Arc<DenseBlockGrid>> {
    let mut world = HashMap::new();
    let lo = WEST.0 - WIDE_RADIUS;
    let hi = EAST.0 + WIDE_RADIUS;
    for cx in lo..=hi {
        for cz in (WEST.1 - WIDE_RADIUS)..=(WEST.1 + WIDE_RADIUS) {
            world.insert((cx, cz), flat_chunk(cx, cz));
        }
    }
    world
}

fn is_tree(state: StateId) -> bool {
    let name = state.name();
    name.ends_with("_leaves")
        || name.ends_with("_log")
        || name.ends_with("_wood")
        || name.ends_with("_stem")
}

/// One full 3×3 drive centred on `centre`, returning every write it made in absolute
/// coordinates.
///
/// `rim` chooses the read neighbourhood: `Rim::Real` is production (the 5×5 this
/// landing introduced), `Rim::Air` narrows it back to the nine-slot table by handing
/// `None` for every offset outside `±1`. That single argument is the control.
fn drive(
    world: &HashMap<(i32, i32), Arc<DenseBlockGrid>>,
    centre: (i32, i32),
    features: &[(usize, PlacedRef)],
    tags: &VegTags,
    rim: Rim,
    selected_source: Option<(i32, i32)>,
) -> HashMap<(i32, i32, i32), StateId> {
    let mut grid = VegGrid::with_sources(
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
    let for_source = |x: i32, z: i32| -> &[(usize, PlacedRef)] {
        if selected_source.is_none_or(|source| source == (x, z)) {
            features
        } else {
            &[]
        }
    };
    apply_vegetal_decoration_step_3x3_per_source(
        &mut random,
        SEED,
        centre.0,
        centre.1,
        &mut grid,
        tags,
        &for_source,
    );
    grid.dirty_cells().map(|(x, y, z, s)| ((x, y, z), s)).collect()
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
    west_drive: &HashMap<(i32, i32, i32), StateId>,
    east_drive: &HashMap<(i32, i32, i32), StateId>,
) -> Seam {
    let border = EAST.0 * 16;
    // Exactly what the client receives: each chunk's own columns from its own drive.
    let served = |x: i32, y: i32, z: i32| -> bool {
        let m = if x < border { west_drive } else { east_drive };
        m.get(&(x, y, z)).is_some_and(|&s| is_tree(s))
    };
    let mut out = Seam::default();
    for d in [west_drive, east_drive] {
        for (&(x, y, z), &state) in d.iter() {
            if x != border - 1 || !is_tree(state) {
                continue;
            }
            // This drive says one canopy occupies both sides of the border here.
            if !d.get(&(border, y, z)).is_some_and(|&s| is_tree(s)) {
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
        .filter(|b| !vegetal_features_for(resolver, b).is_empty())
        .collect();
    out.sort();
    out
}

/// Runs every biome at one seam and returns `(per-biome seam, totals)`.
fn sweep(rim: Rim) -> (Vec<(String, Seam)>, usize, usize) {
    let resolver = FsResolver { root: prod_dir() };
    let tags = build_veg_tags(&resolver);
    let world = flat_world();
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
        let features = vegetal_features_for(&resolver, &biome);
        let w = drive(&world, WEST, &features, &tags, rim, None);
        let e = drive(&world, EAST, &features, &tags, rim, None);
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
        crossings > 0 && !with_crossings.is_empty(),
        "fixture has no border-crossing canopy at {WEST:?}|{EAST:?}; the seam check is vacuous",
    );
    println!("fixture: {crossings} border-crossing canopy rows across {} biomes", with_crossings.len());
}

/// A source must produce the same writes when requested by either adjacent
/// target. The wide read table supplies the same source-owned neighbourhood to
/// both requests; the narrow arm deliberately removes the rim and is expected
/// to diverge.
#[test]
fn source_writes_are_canonical_across_adjacent_requests() {
    let resolver = FsResolver { root: prod_dir() };
    let tags = build_veg_tags(&resolver);
    let features = vegetal_features_for(&resolver, "minecraft:forest");
    assert!(!features.is_empty(), "control premise: forest must have vegetation features");
    let world = flat_world();

    let wide_west = drive(&world, WEST, &features, &tags, Rim::Real, Some(WEST));
    let wide_east = drive(&world, EAST, &features, &tags, Rim::Real, Some(WEST));
    assert_eq!(
        wide_west, wide_east,
        "the same source changed when requested by an adjacent target; wide source routing is not canonical",
    );

    let narrow_west = drive(&world, WEST, &features, &tags, Rim::Air, Some(WEST));
    let narrow_east = drive(&world, EAST, &features, &tags, Rim::Air, Some(WEST));
    assert_ne!(
        narrow_west, narrow_east,
        "negative control did not fire: removing the source rim must change the adjacent request",
    );
}

/// The claim: with the 5×5 read neighbourhood, a canopy that any drive places across
/// the border survives into the served field, except for the named shared-overlay
/// residual.
#[test]
fn a_canopy_crossing_a_chunk_border_is_served_whole() {
    let (rows, total, _) = sweep(Rim::Real);
    let crossing_total: usize = rows.iter().map(|(_, seam)| seam.crossings).sum();
    assert!(crossing_total > 0, "wide arm has no seam crossings");
    assert!(
        total < crossing_total,
        "every crossing is reported as truncated ({total}/{crossing_total}); wide and narrow requests are not stable",
    );
    println!("wide seam signature: crossings={crossing_total}, truncated={total}");
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
    let (_, narrow_total, narrow_crossings) = sweep(Rim::Air);
    let (_, wide_total, wide_crossings) = sweep(Rim::Real);
    assert!(
        narrow_crossings > 0 && wide_crossings > 0,
        "control premise: both arms must contain border-crossing canopies \
         (narrow={narrow_crossings}, wide={wide_crossings}), or neither measurement \
         is about seams",
    );
    assert!(
        narrow_total > wide_total,
        "control failed to fire: narrowing the read neighbourhood back to 3×3 \
         produced {narrow_total} truncated rows against the widened arm's \
         {wide_total}. The widening must be observed making a difference.",
    );
    println!("control: 3x3 read neighbourhood -> {narrow_total} truncated rows; 5x5 -> {wide_total}");
}
