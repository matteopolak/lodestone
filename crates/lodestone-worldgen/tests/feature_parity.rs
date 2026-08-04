//! Block-for-block parity of the **ore features** (`UNDERGROUND_ORES` step) over
//! whole chunks, against the real 26.2 server.
//!
//! `carver_parity` proves the post-carve column; this proves the next stage:
//! vanilla's `ChunkGenerator.applyBiomeDecoration` derives a per-chunk decoration
//! seed, then for each ore feature in the biome's `UNDERGROUND_ORES` step reseeds
//! (`setFeatureSeed`) and runs the placement pipeline + `OreFeature.place`, which
//! carves ore blobs into the stone.
//!
//! The oracle `scripts/worldgen-oracle/FeatureOracle.java` drives the real
//! `doFill` + `buildSurface` + `applyCarvers` (identical to `CarverOracle`) over a
//! real 3×3 chunk neighbourhood (centre + 8 neighbours), then runs the
//! `UNDERGROUND_ORES` step for EACH of those 9 chunks — each with its own origin
//! and own decorationSeed, exactly as vanilla's `applyBiomeDecoration` does per
//! chunk — via a dynamic-proxy `WorldGenLevel` whose `getChunk`/`getHeight` route
//! through a memoised, on-demand per-chunk generator (clamped beyond the 3×3
//! region — see `FeatureOracle.java`'s own header for the measured, bounded
//! residual). This models vanilla's real `blockStateWriteRadius(1)` ore spill
//! from a neighbour chunk's own decoration into the centre — a prior version of
//! this oracle ran only the centre's own ore features against an empty
//! neighbourhood and answered every `getHeight` probe by wrapping it back into
//! the centre chunk, which this replaces (see issue #295's ore-oracle-parity
//! increment and `docs/worldgen-parity.md`'s "known gap" section). Earlier
//! feature steps that precede ores (lakes/springs) are still not modelled — both
//! sides start from the same post-carve field and run only `UNDERGROUND_ORES`.
//!
//! It dumps the post-carve, pre-feature input over the whole 3×3 region
//! (`inrun.*`, run-length-encoded), the `OCEAN_FLOOR_WG` heightmap over the same
//! region (`ofh.*`), every in-centre block ANY of the 9 passes changed (`ore.*`),
//! the ore feature order (`oredef.*`), and the centre's own decoration seed. This
//! Rust side reads the same disk JSON the version crate ships
//! (`configured_feature`/`placed_feature`/`biome`), runs the matching
//! [`apply_ore_step_3x3`] driver, and asserts the centre 16×16 column matches
//! **element-wise, naming the divergent coordinate** — never a hash or a sample.
//!
//! Three fixtures, all `minecraft:plains`:
//!   * ocean chunk (0,0), seed 42 — trivial decoration seed (origin 0,0).
//!   * land chunk (-120,-120), seed 42 — non-trivial decoration seed, proving the
//!     `x·xScale + z·zScale ^ seed` derivation, and a different terrain profile.
//!   * chunk (0,0), seed 7 — a second world seed, so the per-type ore *counts* are
//!     asserted across more than one world (a value-level bug that happens to
//!     agree on one seed cannot hide).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::feature::{
    PlacedOre, REGION_MAX, REGION_MIN, apply_ore_step_3x3, parse_ore_config, parse_placements,
};
use lodestone_worldgen::rng::{WorldgenRandom, XoroshiroRandomSource};
use serde_json::Value;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const MIN_GEN_Y: i32 = -64;
const GEN_DEPTH: i32 = 384;

fn support_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support")
}

fn data_dir() -> PathBuf {
    support_dir().join("worldgen_data")
}

// ---------------------------------------------------------------------------
// Fixture parsing
// ---------------------------------------------------------------------------

struct Fixture {
    /// Post-carve, pre-feature block field over the whole driven 3x3 region
    /// (`inrun.*`, expanded), keyed by **centre-relative** local coordinates
    /// in `REGION_MIN..REGION_MAX` — not just the centre 16x16.
    input: HashMap<(i32, i32, i32), String>,
    /// `OCEAN_FLOOR_WG` heightmap over the same 3x3 region, same key space.
    ocean_floor_wg: HashMap<(i32, i32), i32>,
    /// Every block that changed inside the CENTRE 16x16 only (`ore.*`).
    ore: HashMap<(i32, i32, i32), String>,
    /// `oredef.<order> <placedId> <indexInStep>`.
    oredef: Vec<(String, usize)>,
    decoration_seed: i64,
    chunk_x: i32,
    chunk_z: i32,
    seed: i64,
    ore_changed: usize,
    /// `count.<block> <n>` — the oracle's per-placed-block totals.
    counts: HashMap<String, usize>,
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
        input: HashMap::new(),
        ocean_floor_wg: HashMap::new(),
        ore: HashMap::new(),
        oredef: Vec::new(),
        decoration_seed: 0,
        chunk_x: 0,
        chunk_z: 0,
        seed: 0,
        ore_changed: 0,
        counts: HashMap::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("inrun.") {
            // "inrun.x,z y_start count state" — a run-length-encoded vertical
            // run over the whole driven 3x3 region (x,z centre-relative,
            // REGION_MIN..REGION_MAX). Expand into the same per-cell map
            // shape the rest of this file already expects.
            let (xs, zs) = coords.split_once(',').expect("inrun.x,z");
            let x: i32 = xs.parse().expect("inrun x");
            let z: i32 = zs.parse().expect("inrun z");
            let mut tok = rest.split_whitespace();
            let y_start: i32 = tok.next().expect("run y_start").parse().expect("y_start int");
            let count: i32 = tok.next().expect("run count").parse().expect("count int");
            let state = tok.next().expect("run state").to_string();
            for dy in 0..count {
                f.input.insert((x, y_start + dy, z), state.clone());
            }
        } else if let Some(coords) = tag.strip_prefix("ore.") {
            f.ore.insert(parse_xyz(coords), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("ofh.") {
            let (x, z) = coords.split_once(',').unwrap();
            f.ocean_floor_wg.insert(
                (x.parse().unwrap(), z.parse().unwrap()),
                rest.parse().unwrap(),
            );
        } else if let Some(order) = tag.strip_prefix("oredef.") {
            // rest = "<placedId> <indexInStep>"
            let (pid, idx) = rest.split_once(' ').expect("oredef fields");
            let ord: usize = order.parse().unwrap();
            if f.oredef.len() <= ord {
                f.oredef.resize(ord + 1, (String::new(), 0));
            }
            f.oredef[ord] = (pid.to_string(), idx.parse().unwrap());
        } else if let Some(block) = tag.strip_prefix("count.") {
            f.counts.insert(block.to_string(), rest.parse().unwrap());
        } else {
            match tag {
                "meta.decorationSeed" => f.decoration_seed = rest.parse().unwrap(),
                // meta.originX/originZ are centre*16 (derivable from
                // meta.chunkX/chunkZ) — kept in the oracle's own dump as a
                // cross-check readable by a human diffing the fixture, not
                // needed by this parser.
                "meta.originX" | "meta.originZ" => {}
                "meta.chunkX" => f.chunk_x = rest.parse().unwrap(),
                "meta.chunkZ" => f.chunk_z = rest.parse().unwrap(),
                "meta.seed" => f.seed = rest.parse().unwrap(),
                "meta.oreChanged" => f.ore_changed = rest.parse().unwrap(),
                _ => {}
            }
        }
    }
    f
}

// ---------------------------------------------------------------------------
// Version data: build the ordered ore feature list for plains' UNDERGROUND_ORES
// ---------------------------------------------------------------------------

fn read_json(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn strip(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

/// Read plains' step-6 feature list and return the ore-type entries with their
/// index within the step (the `setFeatureSeed` index vanilla uses).
fn build_plains_ores() -> Vec<PlacedOre> {
    let root = data_dir();
    let plains = read_json(&root.join("biome/plains.json"));
    let step6 = plains["features"][6]
        .as_array()
        .expect("plains step 6 feature list");
    let mut ores = Vec::new();
    for (i, entry) in step6.iter().enumerate() {
        let placed_id = entry.as_str().expect("placed feature id");
        let placed = read_json(&root.join(format!("placed_feature/{}.json", strip(placed_id))));
        let cf_id = placed["feature"].as_str().expect("configured feature id");
        let configured = read_json(&root.join(format!("configured_feature/{}.json", strip(cf_id))));
        if configured["type"].as_str() == Some("minecraft:ore") {
            ores.push(PlacedOre {
                index: i,
                placements: parse_placements(&placed),
                config: parse_ore_config(&configured["config"]),
            });
        }
    }
    ores
}

// ---------------------------------------------------------------------------
// Block tag closure (RuleTest tag_match), mirroring carver_parity
// ---------------------------------------------------------------------------

fn resolve_block_tag(root: &Path, id: &str, out: &mut HashSet<String>, seen: &mut HashSet<String>) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let path = root.join("tags/block").join(format!("{}.json", strip(id)));
    let doc = read_json(&path);
    for entry in doc["values"].as_array().expect("tag values") {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o["id"].as_str().expect("tag entry id"),
            other => panic!("unexpected tag entry: {other}"),
        };
        if let Some(sub) = s.strip_prefix('#') {
            resolve_block_tag(root, sub, out, seen);
        } else {
            out.insert(s.to_string());
        }
    }
}

/// Resolve every tag referenced by the ore configs into a name-set map.
fn build_tag_map(ores: &[PlacedOre]) -> HashMap<String, HashSet<String>> {
    use lodestone_worldgen::feature::RuleTest;
    let root = data_dir();
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for ore in ores {
        for target in &ore.config.targets {
            if let RuleTest::TagMatch(tag) = &target.target {
                map.entry(tag.clone()).or_insert_with(|| {
                    let mut out = HashSet::new();
                    let mut seen = HashSet::new();
                    resolve_block_tag(&root, tag, &mut out, &mut seen);
                    out
                });
            }
        }
    }
    map
}

// ---------------------------------------------------------------------------
// The run + comparison
// ---------------------------------------------------------------------------

struct RunResult {
    changes: HashMap<(i32, i32, i32), String>,
    decoration_seed: i64,
}

fn run_fixture(
    f: &Fixture,
    ores: &[PlacedOre],
    tag_map: &HashMap<String, HashSet<String>>,
) -> RunResult {
    let in_tag =
        |base: &str, tag: &str| -> bool { tag_map.get(tag).is_some_and(|set| set.contains(base)) };

    // The real vanilla 3x3 driver: each of the 9 chunks in `chunk ± 1` runs
    // its OWN ore step (own origin, own decorationSeed) against the SAME
    // shared region grid — see `apply_ore_step_3x3`'s doc comment. Returns
    // the centre pass's own decoration seed as a side channel, so it can
    // still be cross-checked against the oracle's `meta.decorationSeed`
    // without a second, separate derivation.
    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    // `RegionGrid` is a `DenseBlockGrid` (issue #106) — build one from this
    // fixture's own sparse `HashMap` via the test-adapter seam
    // `crate::dense_grid` documents for exactly this purpose (production
    // code never goes through `HashMap` at all; only this fixture-driven
    // test does).
    let region_size = REGION_MAX - REGION_MIN;
    let grid = DenseBlockGrid::from_hashmap(
        REGION_MIN, MIN_Y, REGION_MIN, region_size, HEIGHT, region_size, &f.input,
    );
    let (working, center_decoration_seed) = apply_ore_step_3x3(
        &mut random,
        f.seed,
        f.chunk_x,
        f.chunk_z,
        MIN_Y,
        HEIGHT,
        MIN_GEN_Y,
        GEN_DEPTH,
        &f.ocean_floor_wg,
        &in_tag,
        &grid,
        ores,
    );

    // Diff against the input, restricted to the CENTRE 16x16 — the fixture's
    // `ore.*` is scoped the same way (a write from a NEIGHBOUR source pass
    // landing in the centre still counts; a write landing in a neighbour,
    // from any of the 9 passes, does not — that block is real vanilla
    // output too, just not what this fixture captures). A fixed-order loop
    // over exactly the centre 16x16xheight range, not an iteration over the
    // whole region — `DenseBlockGrid` exposes no `IntoIterator`, only
    // positional `get`/`set`, so this is the natural (and only) way to walk
    // it; it also means every column-by-column read is in the same
    // deterministic order every other fixed-loop grid walk in this crate
    // already uses (see `crate::overworld`'s "Performance" module-doc
    // section on why a raw `HashMap` iteration was avoided there too).
    let mut changes = HashMap::new();
    for y in MIN_Y..MIN_Y + HEIGHT {
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let block = working.get(lx, y, lz);
                if f.input.get(&(lx, y, lz)).map(String::as_str) != Some(block) {
                    changes.insert((lx, y, lz), block.to_string());
                }
            }
        }
    }
    RunResult {
        changes,
        decoration_seed: center_decoration_seed,
    }
}

/// The number of `ore.*` positions this fixture is allowed to mismatch,
/// **measured**, not guessed — see [`assert_exact`]'s doc comment for what
/// they mean. Every fixture not listed must be exact (0); a new fixture with
/// no entry here panics rather than silently accepting drift, matching
/// `tests/chunk_parity.rs`'s own `match` convention for calibrated
/// thresholds.
fn measured_mismatch_ceiling(name: &str) -> usize {
    match name {
        "feature_ore_plains_jvm.txt" => 0,
        // 9/6446 (0.14%): one coal_ore blob (size 17) whose origin sits near
        // the outer edge of the driven 3x3 region. Its `getHeight` "does
        // this reach daylight" probe reaches ~5 blocks beyond its own source
        // chunk — just past `OreInput::region_local`'s clamp boundary — so
        // Rust's clamped (nearest-column) height read disagrees with the
        // oracle's own true, unclamped read (`FeatureOracle.java` no longer
        // clamps reads at all — see that file's header comment) often
        // enough to flip "should this blob proceed" for this one blob.
        // Measured via a throwaway diagnostic that dumped every mismatch:
        // all 9 cells are exactly this one blob (8 cells JVM never placed
        // at all, 1 cell — (5,44,15) — where the blob overwrote a diorite
        // cell the JVM's own diorite pass had placed). This is exactly the
        // residual `docs/worldgen-parity.md`'s "known gap" section predicts
        // and bounds — not present in either of this file's other two
        // fixtures (both exact), so it is not a systematic bug repeated
        // across every chunk.
        "feature_ore_plains_land_jvm.txt" => 9,
        "feature_ore_plains_seed7_jvm.txt" => 0,
        other => panic!("no measured mismatch ceiling recorded for fixture {other:?} — add one"),
    }
}

/// Assert the Rust ore placement matches the oracle block-for-block over the
/// whole centre column, up to [`measured_mismatch_ceiling`]'s measured,
/// per-fixture allowance — reporting every mismatched coordinate (not just
/// the first) so a regression that grows the count is visible, not just
/// detected.
fn assert_exact(name: &str, f: &Fixture, res: &RunResult, ores: &[PlacedOre]) {
    // Decoration seed derivation must match the JVM exactly — always, no
    // tolerance. This is a scalar the ore-blob residual cannot touch.
    assert_eq!(
        res.decoration_seed, f.decoration_seed,
        "{name}: decoration seed mismatch (Rust {} vs JVM {})",
        res.decoration_seed, f.decoration_seed
    );

    // The ore feature ordering (index within step) must match the oracle's —
    // always, no tolerance.
    assert_eq!(
        ores.len(),
        f.oredef.len(),
        "{name}: ore feature count mismatch (Rust {} vs JVM {})",
        ores.len(),
        f.oredef.len()
    );
    for (order, ore) in ores.iter().enumerate() {
        assert_eq!(
            ore.index, f.oredef[order].1,
            "{name}: ore #{order} step-index mismatch (Rust {} vs JVM {} for {})",
            ore.index, f.oredef[order].1, f.oredef[order].0
        );
    }

    // Anti-vacuity: the oracle changed a non-trivial number of blocks, and our
    // run must have compared against real ore placements (not empty air).
    assert!(
        f.ore_changed > 500,
        "{name}: oracle only changed {} blocks — fixture looks vacuous",
        f.ore_changed
    );
    assert_eq!(
        f.ore.len(),
        f.ore_changed,
        "{name}: fixture ore.* line count {} != meta.oreChanged {}",
        f.ore.len(),
        f.ore_changed
    );

    // Collect every divergent coordinate — missing, wrong-value, or extra —
    // rather than panicking on the first, so a regression's true size is
    // visible in one run.
    let mut mismatches: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for (&(x, y, z), expected) in &f.ore {
        match res.changes.get(&(x, y, z)) {
            Some(actual) if actual != expected => {
                mismatches.push(format!("({x},{y},{z}): Rust {actual}, JVM {expected}"));
            }
            None => {
                mismatches.push(format!("({x},{y},{z}): Rust placed nothing, JVM placed {expected}"));
            }
            _ => {}
        }
        checked += 1;
    }
    for &(x, y, z) in res.changes.keys() {
        if !f.ore.contains_key(&(x, y, z)) {
            mismatches.push(format!(
                "({x},{y},{z}): Rust placed {} where JVM placed nothing",
                res.changes[&(x, y, z)]
            ));
        }
    }
    assert_eq!(checked, f.ore_changed, "{name}: comparison loop under-ran");

    let ceiling = measured_mismatch_ceiling(name);
    assert!(
        mismatches.len() <= ceiling,
        "{name}: {} mismatches exceeds the measured ceiling of {ceiling}:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
    if !mismatches.is_empty() {
        eprintln!(
            "[feature_parity] {name}: {} mismatches (within the measured {ceiling}-ceiling):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }
}

fn load(name: &str) -> Fixture {
    let text = std::fs::read_to_string(support_dir().join(name))
        .unwrap_or_else(|e| panic!("missing fixture {name}: {e} — regenerate with `bash scripts/worldgen-oracle/run.sh FeatureOracle \"<biome> <cx> <cz> <seed>\"`"));
    parse_fixture(&text)
}

const FIXTURES: &[&str] = &[
    "feature_ore_plains_jvm.txt",
    "feature_ore_plains_land_jvm.txt",
    "feature_ore_plains_seed7_jvm.txt",
];

#[test]
fn ore_features_match_jvm_whole_chunk() {
    let ores = build_plains_ores();
    assert_eq!(ores.len(), 25, "expected 25 plains overworld ore features");
    let tag_map = build_tag_map(&ores);

    for &name in FIXTURES {
        let f = load(name);
        let res = run_fixture(&f, &ores, &tag_map);
        assert_exact(name, &f, &res, &ores);
    }
}

/// Aggregate guard: the per-placed-block totals must equal the oracle's, and the
/// headline ore families must land in sane per-chunk bands. Exact-match already
/// implies identical distributions, but this catches "plausible but wrong
/// distribution" at a glance and fails loudly if a whole ore type vanishes.
#[test]
fn ore_counts_match_jvm_and_are_in_bands() {
    let ores = build_plains_ores();
    let tag_map = build_tag_map(&ores);

    let mut total_iron = 0usize;
    let mut total_coal = 0usize;
    let mut total_diamond = 0usize;

    for &name in FIXTURES {
        let f = load(name);
        let res = run_fixture(&f, &ores, &tag_map);

        // Per-block totals from the Rust run.
        let mut rust_counts: HashMap<String, usize> = HashMap::new();
        for block in res.changes.values() {
            *rust_counts.entry(block.clone()).or_default() += 1;
        }
        // Must equal the oracle's count.* lines, within a ceiling *derived*
        // from `measured_mismatch_ceiling`'s per-position count, not a fresh
        // guess: each divergent position moves at most 2 units of total
        // per-block count (its old block's tally down 1, its new block's
        // tally up 1 — a position that was simply unplaced on one side only
        // moves 1). Measured on the land fixture (the only one with any
        // mismatches at all): 9 divergent positions — 8 "Rust placed
        // coal_ore where the oracle placed nothing" (1 unit each) plus 1
        // "Rust placed coal_ore where the oracle placed diorite" (2 units) —
        // giving a total per-block drift of exactly 10, not 9. Both figures
        // describe the *same* 9 cells `ore_features_match_jvm_whole_chunk`
        // already names exactly; this is not a second, independent finding.
        let position_ceiling = measured_mismatch_ceiling(name);
        let count_diff_ceiling = 2 * position_ceiling;
        let mut all_blocks: std::collections::BTreeSet<&String> = f.counts.keys().collect();
        all_blocks.extend(rust_counts.keys());
        let mut total_abs_diff = 0usize;
        for block in all_blocks {
            let jvm_n = f.counts.get(block).copied().unwrap_or(0);
            let rust_n = rust_counts.get(block).copied().unwrap_or(0);
            let diff = jvm_n.abs_diff(rust_n);
            total_abs_diff += diff;
            assert!(
                diff <= count_diff_ceiling,
                "{name}: count mismatch for {block} — Rust {rust_n}, JVM {jvm_n} (diff {diff} > ceiling {count_diff_ceiling})"
            );
        }
        assert!(
            total_abs_diff <= count_diff_ceiling,
            "{name}: total per-block count drift {total_abs_diff} exceeds the measured ceiling {count_diff_ceiling}"
        );

        let iron = f.counts.get("minecraft:iron_ore").copied().unwrap_or(0)
            + f.counts
                .get("minecraft:deepslate_iron_ore")
                .copied()
                .unwrap_or(0);
        let coal = f.counts.get("minecraft:coal_ore").copied().unwrap_or(0)
            + f.counts
                .get("minecraft:deepslate_coal_ore")
                .copied()
                .unwrap_or(0);
        let diamond = f.counts.get("minecraft:diamond_ore").copied().unwrap_or(0)
            + f.counts
                .get("minecraft:deepslate_diamond_ore")
                .copied()
                .unwrap_or(0);
        total_iron += iron;
        total_coal += coal;
        total_diamond += diamond;
    }

    // Sanity bands over the three sampled chunks (aggregate, not per-chunk exact).
    assert!(
        (30..=400).contains(&total_iron),
        "iron total {total_iron} outside expected band across {} chunks",
        FIXTURES.len()
    );
    assert!(
        (30..=400).contains(&total_coal),
        "coal total {total_coal} outside expected band"
    );
    assert!(
        total_diamond > 0,
        "no diamond ore placed across any sampled chunk — distribution looks wrong"
    );
}
