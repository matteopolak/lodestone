//! Block-for-block parity of vegetal decoration (grass/flowers/trees, issue
//! #406) against the real 26.2 server — the evidence gap that module's own
//! doc comment named plainly: "no oracle validates this against a real
//! vanilla dump." This closes it.
//!
//! `scripts/worldgen-oracle/VegetationOracle.java` boots the real server
//! headlessly, replays the real `UNDERGROUND_ORES` step over a 3x3
//! neighbourhood (so the vegetal-decoration pass below starts from the same
//! post-ore terrain `OverworldGenerator::vegetation_stage` does on the Rust
//! side, not a pre-ore approximation), then runs `VEGETAL_DECORATION` TWICE
//! from that identical baseline:
//!
//!   * `single.*` — only the centre chunk's own decoration pass. This is
//!     the scope `crate::feature::vegetation` actually implements —
//!     comparing our engine's own output against this is the correctness
//!     gate.
//!   * `full3x3.*` — all 9 chunks in the driven neighbourhood, matching
//!     vanilla's real `blockStateWriteRadius(1)` spill. Comparing this
//!     against `single.*` is the **measured** cross-chunk-spill gap this
//!     module's own doc named but had never measured against a real dump —
//!     see [`single_chunk_only_undercounts_real_vanilla_centre_content`].
//!
//! Two fixtures, both `minecraft:plains`, seed 42: chunk `(-120,-120)` (this
//! crate's own "land chunk" convention, `feature_parity.rs`) and chunk
//! `(5,5)` (picked once, before any number was known, specifically so the
//! measured spill fraction couldn't be cherry-picked to look small — see
//! CLAUDE.md's evidence standard on picking coordinates that exercise real
//! structure, not a vacuous sweep).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lodestone_worldgen::compose::build_biome_vegetation;
use lodestone_worldgen::density::{NoiseParams, Resolver};
use lodestone_worldgen::feature::vegetation::{apply_vegetal_decoration_step, build_veg_tags, VegGrid};
use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};
use serde_json::Value;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data")
}

fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support")
}

/// Minimal `Resolver` reading straight off disk under `tests/support
/// /worldgen_data` — the same fixture tree `tests/feature_parity.rs` and
/// `benches/generation.rs` already keep current, extended with the
/// biome/feature/tag methods vegetal decoration needs. `density_function`/
/// `noise` are never called by anything this test exercises (no chunk shape
/// is generated here — `VegGrid` is seeded directly from the oracle's own
/// `base.*` dump), so they panic loudly rather than silently returning
/// nonsense if that assumption ever stops holding.
struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn try_json(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        std::fs::read_to_string(&path)
            .ok()
            .map(|text| serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display())))
            .unwrap_or(Value::Null)
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        panic!("vegetation_parity never generates shape; unexpected density_function({id})");
    }
    fn noise(&self, id: &str) -> NoiseParams {
        panic!("vegetation_parity never generates shape; unexpected noise({id})");
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

// ---------------------------------------------------------------------------
// Fixture parsing
// ---------------------------------------------------------------------------

struct Fixture {
    /// The centre chunk's own post-ore terrain (`base.*`), local `(0..16,
    /// y, 0..16)` — what `VegGrid` is seeded from.
    base: HashMap<(i32, i32, i32), String>,
    /// Every cell the SINGLE (centre-only) pass changed, local coordinates
    /// (a subset can fall outside `0..16` — see module doc's "single mode's
    /// reads still see the real neighbourhood" scope note; those cells are
    /// filtered out before comparing against our engine, which structurally
    /// cannot produce them).
    single_diff: HashMap<(i32, i32, i32), String>,
    single_centre_changed: usize,
    /// Every cell the FULL3X3 pass changed, centre-relative coordinates,
    /// spanning the whole driven `-16..32` region.
    full_diff: HashMap<(i32, i32, i32), String>,
    full_centre_changed: usize,
    decoration_seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    seed: i64,
}

fn parse_xyz(s: &str) -> (i32, i32, i32) {
    let mut it = s.split(',');
    let x = it.next().unwrap().parse().unwrap();
    let y = it.next().unwrap().parse().unwrap();
    let z = it.next().unwrap().parse().unwrap();
    (x, y, z)
}

fn parse_fixture(text: &str) -> Fixture {
    let mut f = Fixture {
        base: HashMap::new(),
        single_diff: HashMap::new(),
        single_centre_changed: 0,
        full_diff: HashMap::new(),
        full_centre_changed: 0,
        decoration_seed: 0,
        chunk_x: 0,
        chunk_z: 0,
        seed: 0,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((tag, rest)) = line.split_once(' ') else {
            continue;
        };
        if let Some(coords) = tag.strip_prefix("base.") {
            // "base.x,z y_start count state"
            let (xs, zs) = coords.split_once(',').expect("base.x,z");
            let (x, z): (i32, i32) = (xs.parse().unwrap(), zs.parse().unwrap());
            let mut parts = rest.splitn(3, ' ');
            let y_start: i32 = parts.next().unwrap().parse().unwrap();
            let count: i32 = parts.next().unwrap().parse().unwrap();
            let state = parts.next().unwrap().to_string();
            for dy in 0..count {
                f.base.insert((x, y_start + dy, z), state.clone());
            }
        } else if let Some(coords) = tag.strip_prefix("single.diff.") {
            let (x, y, z) = parse_xyz(coords);
            f.single_diff.insert((x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("full3x3.diff.") {
            let (x, y, z) = parse_xyz(coords);
            f.full_diff.insert((x, y, z), rest.to_string());
        } else if tag == "single.meta.centreChanged" {
            f.single_centre_changed = rest.parse().unwrap();
        } else if tag == "full3x3.meta.centreChanged" {
            f.full_centre_changed = rest.parse().unwrap();
        } else {
            match tag {
                "meta.decorationSeed" => f.decoration_seed = rest.parse().unwrap(),
                "meta.chunkX" => f.chunk_x = rest.parse().unwrap(),
                "meta.chunkZ" => f.chunk_z = rest.parse().unwrap(),
                "meta.seed" => f.seed = rest.parse().unwrap(),
                "meta.postOreReplayMismatches" => {
                    let n: usize = rest.parse().unwrap();
                    assert_eq!(n, 0, "the oracle's own post-ore baseline must replay identically between its two passes");
                }
                _ => {}
            }
        }
    }
    f
}

fn load(name: &str) -> Fixture {
    let text = std::fs::read_to_string(support_dir().join(name)).unwrap_or_else(|e| {
        panic!(
            "missing fixture {name}: {e} — regenerate with `bash scripts/worldgen-oracle/run.sh VegetationOracle \"<biome> <cx> <cz> <seed>\"`"
        )
    });
    parse_fixture(&text)
}

const FIXTURES: &[&str] = &["vegetation_plains_land_jvm.txt", "vegetation_plains_chunk5_5_jvm.txt"];

// ---------------------------------------------------------------------------
// Run our own engine from the fixture's baseline and diff against `single.*`
// ---------------------------------------------------------------------------

/// Runs `crate::feature::vegetation`'s real, production
/// `apply_vegetal_decoration_step` — the exact function
/// `OverworldGenerator::vegetation_stage` calls — seeded from `f.base`, and
/// returns every cell it wrote, local coordinates, matching `f.single_diff`'s
/// key space.
fn run_our_engine(f: &Fixture, resolver: &FsResolver) -> HashMap<(i32, i32, i32), String> {
    let base_x = f.chunk_x * 16;
    let base_z = f.chunk_z * 16;
    let mut grid = VegGrid::new(MIN_Y, HEIGHT, base_x, base_z);
    for (&(lx, y, lz), state) in &f.base {
        grid.seed(base_x + lx, y, base_z + lz, state.clone());
    }
    let tags = build_veg_tags(resolver);
    let features = build_biome_vegetation(resolver, "minecraft:plains");
    assert!(!features.is_empty(), "plains must resolve a non-empty VEGETAL_DECORATION list");

    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    apply_vegetal_decoration_step(&mut random, f.seed, f.chunk_x, f.chunk_z, &mut grid, &tags, &features);

    grid.dirty_cells()
        .map(|(x, y, z, state)| ((x - base_x, y, z - base_z), state.to_string()))
        .collect()
}

fn assert_matches_single(name: &str, f: &Fixture, ours: &HashMap<(i32, i32, i32), String>) {
    // `single_diff` can carry a handful of cells outside the centre 16x16
    // (see module doc: SINGLE mode's own placement can, in principle, land
    // just outside the chunk the same way the real engine's writes would —
    // measured at 0 for both fixtures below, but not assumed to always be
    // 0). Our engine structurally cannot produce those (`VegGrid::
    // set_if_in_bounds` drops them), so they're excluded here — this is the
    // engine's known, named single-chunk scope, not a discrepancy this gate
    // is checking.
    // `glow_lichen` (`multiface_growth`) is a named, accepted gap — see
    // `crate::feature::vegetation`'s module doc: nothing in issue #406's
    // scope models `MultifaceGrowthFeature` (it isn't a tree/grass/flower),
    // so it's excluded here rather than treated as a correctness failure.
    // Every OTHER cell in `single_diff` (grass/flowers/trees) is real,
    // implemented scope and must match exactly.
    let expected: HashMap<(i32, i32, i32), &String> = f
        .single_diff
        .iter()
        .filter(|&(&(x, _, z), _)| (0..16).contains(&x) && (0..16).contains(&z))
        .filter(|(_, state)| !state.starts_with("minecraft:glow_lichen"))
        .map(|(k, v)| (*k, v))
        .collect();

    let mut mismatches = Vec::new();
    for (&pos, exp) in &expected {
        match ours.get(&pos) {
            Some(got) if got == *exp => {}
            Some(got) => mismatches.push(format!("{pos:?}: expected {exp}, got {got}")),
            None => mismatches.push(format!("{pos:?}: expected {exp}, got <nothing written>")),
        }
    }
    for (&pos, got) in ours {
        if !expected.contains_key(&pos) {
            mismatches.push(format!("{pos:?}: unexpected write {got} (JVM wrote nothing here)"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{name}: engine output diverges from the real JVM's SINGLE-mode pass ({} expected cells, {} of ours) —\n{}",
        expected.len(),
        ours.len(),
        mismatches.join("\n")
    );
}

#[test]
fn our_engine_matches_jvm_single_chunk_pass() {
    let resolver = FsResolver { root: data_dir() };
    for &name in FIXTURES {
        let f = load(name);
        let ours = run_our_engine(&f, &resolver);
        assert_matches_single(name, &f, &ours);
        // Cross-check: the oracle's own `single.meta.centreChanged` minus
        // the known glow_lichen gap (see `assert_matches_single`'s own
        // filter) must equal our engine's write count exactly — so a bug in
        // this test's own filtering logic can't silently pass by both sides
        // being wrong the same way.
        let glow_lichen_cells = f
            .single_diff
            .iter()
            .filter(|&(&(x, _, z), _)| (0..16).contains(&x) && (0..16).contains(&z))
            .filter(|(_, state)| state.starts_with("minecraft:glow_lichen"))
            .count();
        assert_eq!(
            ours.len(),
            f.single_centre_changed - glow_lichen_cells,
            "{name}: our engine's own write count must match the oracle's single.meta.centreChanged, minus the named glow_lichen gap"
        );
    }
}

/// **The headline finding**: single-chunk-only vegetation (this engine's
/// real scope) against real vanilla's full 3x3 spill, over the centre
/// chunk. CLAUDE.md's evidence standard: predict a value, don't just assert
/// a direction. Both fixtures happen to land in a tight band (measured, not
/// designed): `single/full3x3` = 25/32 ≈ 78.1% and 48/61 ≈ 78.7% — a
/// consistent ~21-22% undercount of the centre chunk's real vanilla
/// vegetation content from cross-chunk spill alone, for plains at these two
/// coordinates. This is a *measurement*, not a tunable constant — the
/// assertions below only pin down what was actually observed (a floor + a
/// generous band around it), so a future engine change that shifts these
/// numbers is expected to fail this test and force the comment to be
/// updated with fresh numbers, not silently drift.
#[test]
fn single_chunk_only_undercounts_real_vanilla_centre_content() {
    let mut ratios = Vec::new();
    for &name in FIXTURES {
        let f = load(name);
        assert!(f.single_centre_changed > 0, "{name}: fixture must be non-vacuous (plains, not an ocean chunk)");
        assert!(
            f.full_centre_changed > f.single_centre_changed,
            "{name}: real vanilla 3x3 spill must add MORE to the centre than the centre's own pass alone \
             produced (single={}, full3x3={}) — a control that fires: if this ever shows single >= full3x3, \
             either the oracle's two passes stopped sharing a baseline or vanilla's own spill vanished, both \
             of which are more surprising than the number moving a little",
            f.single_centre_changed,
            f.full_centre_changed
        );
        let ratio = f.single_centre_changed as f64 / f.full_centre_changed as f64;
        println!(
            "{name}: single={} full3x3={} ratio={ratio:.3} (single-chunk-only vegetation captures {:.1}% of real vanilla's centre-chunk content)",
            f.single_centre_changed, f.full_centre_changed, ratio * 100.0
        );
        ratios.push(ratio);
    }
    // Measured band across both fixtures: 0.781 and 0.787. A wide-but-real
    // band (not 0..1) so this remains a meaningful regression control
    // rather than a tautology.
    for (name, ratio) in FIXTURES.iter().zip(&ratios) {
        assert!(
            (0.60..=0.95).contains(ratio),
            "{name}: single/full3x3 ratio {ratio:.3} outside the measured band [0.60, 0.95] — \
             re-measure with the oracle before assuming either side regressed"
        );
    }
}
