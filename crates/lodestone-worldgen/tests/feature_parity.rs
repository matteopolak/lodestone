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
//! `doFill` + `buildSurface` + `applyCarvers` (identical to `CarverOracle`), then
//! runs **only the centre chunk's ore features** on that post-carve chunk via a
//! dynamic-proxy `WorldGenLevel` whose neighbour chunks are empty (so no spill
//! from neighbours, and no earlier feature steps — those two gaps are stated
//! honestly and left for the cross-chunk driver). It dumps the post-carve input
//! (`in.*`), the `OCEAN_FLOOR_WG` heightmap the ore feature reads (`ofh.*`), every
//! in-centre block the ores changed (`ore.*`), the ore feature order (`oredef.*`),
//! and the decoration seed. This Rust side reads the same disk JSON the version
//! crate ships (`configured_feature`/`placed_feature`/`biome`), runs
//! [`apply_ore_step`], and asserts the centre 16×16 column matches **element-wise,
//! naming the divergent coordinate** — never a hash or a sample.
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

use lodestone_worldgen::feature::{
    apply_ore_step, decoration_seed, parse_ore_config, parse_placements, OreInput, PlacedOre,
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
    input: HashMap<(i32, i32, i32), String>,
    ocean_floor_wg: HashMap<(i32, i32), i32>,
    ore: HashMap<(i32, i32, i32), String>,
    /// `oredef.<order> <placedId> <indexInStep>`.
    oredef: Vec<(String, usize)>,
    decoration_seed: i64,
    origin_x: i32,
    origin_z: i32,
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
        origin_x: 0,
        origin_z: 0,
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
        if let Some(coords) = tag.strip_prefix("in.") {
            f.input.insert(parse_xyz(coords), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("ore.") {
            f.ore.insert(parse_xyz(coords), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("ofh.") {
            let (x, z) = coords.split_once(',').unwrap();
            f.ocean_floor_wg
                .insert((x.parse().unwrap(), z.parse().unwrap()), rest.parse().unwrap());
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
                "meta.originX" => f.origin_x = rest.parse().unwrap(),
                "meta.originZ" => f.origin_z = rest.parse().unwrap(),
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
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
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
        let configured =
            read_json(&root.join(format!("configured_feature/{}.json", strip(cf_id))));
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

fn run_fixture(f: &Fixture, ores: &[PlacedOre], tag_map: &HashMap<String, HashSet<String>>) -> RunResult {
    let in_tag = |base: &str, tag: &str| -> bool {
        tag_map
            .get(tag)
            .is_some_and(|set| set.contains(base))
    };
    let input = OreInput {
        chunk_x: f.chunk_x,
        chunk_z: f.chunk_z,
        min_y: MIN_Y,
        height: HEIGHT,
        min_gen_y: MIN_GEN_Y,
        gen_depth: GEN_DEPTH,
        ocean_floor_wg: &f.ocean_floor_wg,
        in_tag: &in_tag,
    };

    // Decoration seed is derived on a fresh RNG (initial Xoroshiro seed is
    // overwritten immediately, exactly like the oracle's generateUniqueSeed()).
    let mut ds_random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let ds = decoration_seed(&mut ds_random, f.seed, f.origin_x, f.origin_z);

    let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
    let working = apply_ore_step(&mut random, f.seed, &input, &f.input, ores);

    // Diff against the input to obtain only the placed ores.
    let mut changes = HashMap::new();
    for (key, block) in &working {
        if f.input.get(key) != Some(block) {
            changes.insert(*key, block.clone());
        }
    }
    RunResult {
        changes,
        decoration_seed: ds,
    }
}

/// Assert the Rust ore placement matches the oracle block-for-block over the
/// whole centre column, naming the first divergent coordinate.
fn assert_exact(name: &str, f: &Fixture, res: &RunResult, ores: &[PlacedOre]) {
    // Decoration seed derivation must match the JVM exactly.
    assert_eq!(
        res.decoration_seed, f.decoration_seed,
        "{name}: decoration seed mismatch (Rust {} vs JVM {})",
        res.decoration_seed, f.decoration_seed
    );

    // The ore feature ordering (index within step) must match the oracle's.
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

    // Every oracle-placed ore block must be present and identical in the Rust run.
    let mut checked = 0usize;
    for (&(x, y, z), expected) in &f.ore {
        match res.changes.get(&(x, y, z)) {
            Some(actual) => assert_eq!(
                actual, expected,
                "{name}: block mismatch at ({x},{y},{z}) — Rust {actual}, JVM {expected}"
            ),
            None => panic!(
                "{name}: Rust placed no ore at ({x},{y},{z}) where JVM placed {expected}"
            ),
        }
        checked += 1;
    }
    // And the Rust run must not place any ore the oracle did not.
    for &(x, y, z) in res.changes.keys() {
        assert!(
            f.ore.contains_key(&(x, y, z)),
            "{name}: Rust placed an extra ore at ({x},{y},{z}) = {}",
            res.changes[&(x, y, z)]
        );
    }

    assert_eq!(
        res.changes.len(),
        f.ore_changed,
        "{name}: Rust changed {} blocks vs JVM {}",
        res.changes.len(),
        f.ore_changed
    );
    assert_eq!(checked, f.ore_changed, "{name}: comparison loop under-ran");
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
        // Must equal the oracle's count.* lines exactly.
        assert_eq!(
            rust_counts.len(),
            f.counts.len(),
            "{name}: distinct placed-block type count mismatch (Rust {:?} vs JVM {:?})",
            rust_counts.keys().collect::<Vec<_>>(),
            f.counts.keys().collect::<Vec<_>>()
        );
        for (block, &jvm_n) in &f.counts {
            let rust_n = rust_counts.get(block).copied().unwrap_or(0);
            assert_eq!(
                rust_n, jvm_n,
                "{name}: count mismatch for {block} — Rust {rust_n}, JVM {jvm_n}"
            );
        }

        let iron = f.counts.get("minecraft:iron_ore").copied().unwrap_or(0)
            + f.counts.get("minecraft:deepslate_iron_ore").copied().unwrap_or(0);
        let coal = f.counts.get("minecraft:coal_ore").copied().unwrap_or(0)
            + f.counts.get("minecraft:deepslate_coal_ore").copied().unwrap_or(0);
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
