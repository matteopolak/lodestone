//! Block-for-block parity of the **surface rules** over whole chunks.
//!
//! `chunk_parity` proves the interpolated `final_density` field; this test
//! proves the next stage: vanilla's own surface-building scan scans each
//! column, tracking stone/water depth, and rewrites the top `defaultBlock`
//! (stone) run into grass/dirt/sand/gravel/bedrock/deepslate per the
//! data-driven `surface_rule` tree.
//!
//! The oracle `scripts/worldgen-oracle/SurfaceOracle.java` drives the real 26.2
//! `doFill` + `buildSurface` for a chosen chunk at seed 42 with the biome
//! pinned (via `FixedBiomeSource` + a fixed `BiomeManager`, decoupling this from
//! the not-yet-built multi-noise biome source) and dumps the pre-surface column,
//! the post-surface column, the `WORLD_SURFACE_WG` heightmap, and a
//! canonicalisation table for every result state. [`SurfaceSystem`] consumes the
//! pre-surface column + heightmap and must reproduce the post-surface column
//! exactly — 16×16×384 = 98304 blocks per chunk.
//!
//! Two fixtures, both `minecraft:plains`:
//!   * `surface_plains_jvm.txt`      — chunk (0,0), fully oceanic (hm ≡ 62):
//!     exercises bedrock floor (positional RNG), deepslate/stone
//!     `vertical_gradient`, `above_preliminary_surface`, water/stone_depth/hole
//!     and the ocean-floor dirt/gravel banding.
//!   * `surface_plains_land_jvm.txt` — chunk (-120,-120), land (hm→109):
//!     additionally exercises the visible land banding — `grass_block`, `dirt`,
//!     the stone-vs-gravel bottom rule, and lava pockets.
//!
//! Not yet exercised (tracked separately): sand beaches, badlands `bandlands`
//! and other biome-specific branches, which need other biomes / positions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lodestone_worldgen::density::{Builder, NoiseParams, Resolver};
use lodestone_worldgen::interner::StateInterner;
use lodestone_worldgen::surface::{BlockCanon, PreState, SurfaceSystem};
use serde_json::Value;

const SEED: i64 = 42;
const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
    }
}

impl Resolver for FsResolver {
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
}

fn key(x: i32, y: i32, z: i32) -> (i32, i32, i32) {
    (x, y, z)
}

struct Reference {
    pre: HashMap<(i32, i32, i32), String>,
    post: HashMap<(i32, i32, i32), String>,
    hm: HashMap<(i32, i32), i32>,
    canon: BlockCanon,
    biome: String,
    chunk_x: i32,
    chunk_z: i32,
}

fn parse_reference(text: &str) -> Reference {
    let mut r = Reference {
        pre: HashMap::new(),
        post: HashMap::new(),
        hm: HashMap::new(),
        canon: HashMap::new(),
        biome: String::new(),
        chunk_x: 0,
        chunk_z: 0,
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("pre.") {
            let (x, y, z) = parse_xyz(coords);
            r.pre.insert(key(x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("post.") {
            let (x, y, z) = parse_xyz(coords);
            r.post.insert(key(x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("hm.") {
            let (x, z) = coords.split_once(',').expect("hm x,z");
            r.hm.insert(
                (x.parse().unwrap(), z.parse().unwrap()),
                rest.parse().unwrap(),
            );
        } else if let Some(part_key) = tag.strip_prefix("canonmap.") {
            r.canon.insert(part_key.to_string(), rest.to_string());
        } else if tag == "meta.biome" {
            r.biome = rest.to_string();
        } else if tag == "meta.chunkX" {
            r.chunk_x = rest.parse().unwrap();
        } else if tag == "meta.chunkZ" {
            r.chunk_z = rest.parse().unwrap();
        }
    }

    r
}

fn parse_xyz(s: &str) -> (i32, i32, i32) {
    let mut it = s.split(',');
    let x = it.next().unwrap().parse().unwrap();
    let y = it.next().unwrap().parse().unwrap();
    let z = it.next().unwrap().parse().unwrap();
    (x, y, z)
}

fn run_fixture(label: &str, text: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();

    let r = parse_reference(text);
    assert_eq!(r.biome, "minecraft:plains", "fixture biome");

    let builder = Builder::new(SEED, &resolver);
    // U21: `SurfaceSystem` speaks interned `StateId` now. This fixture path
    // deliberately goes through `PreState::from_name`, which classifies from the
    // *string* (`class_of_name`) exactly as the pre-U21 scan did — so what this
    // test drives is the same classification logic against the same JVM-dumped
    // expected values, not the `BlockKind` shortcut production takes. The
    // shortcut's agreement with this path is asserted separately, at the
    // production seam in `overworld/fill.rs::surface_stage`.
    let interner = Arc::new(StateInterner::new());
    let surface = SurfaceSystem::new(&settings, &builder, &r.canon, &interner);

    let pre_fn = |x: i32, y: i32, z: i32| -> PreState {
        match r.pre.get(&key(x, y, z)) {
            Some(name) => PreState::from_name(&interner, name),
            None => PreState::AIR,
        }
    };
    let hm_fn = |x: i32, z: i32| -> i32 { *r.hm.get(&(x, z)).expect("heightmap") };
    // plains is not "cold enough to snow"; the temperature condition is only
    // reached inside snowy/frozen biome branches, none of which match plains.
    // Fixed for the whole fixture (biome became a runtime input, but
    // this fixture — like the JVM dump it compares against — only ever ran
    // under one biome).
    let biome_at = |_x: i32, _y: i32, _z: i32| -> (&str, bool) { (r.biome.as_str(), false) };

    // `build_surface` now returns a sparse diff (only positions a surface rule
    // actually rewrote — see its doc comment); a position absent from it is
    // unchanged from the pre-surface column, i.e. `pre_fn(x, y, z)`. This test
    // still compares the reconstructed *full* column against the JVM dump
    // block-for-block, same as before the diff change — only how the "no
    // rewrite" case is looked up differs.
    let result = surface.build_surface(&pre_fn, &hm_fn, &biome_at, r.chunk_x * 16, r.chunk_z * 16);

    let mut total = 0usize;
    let mut matching = 0usize;
    let mut first_divergence: Option<(i32, i32, i32, String, String)> = None;

    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..(MIN_Y + HEIGHT) {
                total += 1;
                let want = r.post.get(&key(x, y, z)).expect("post block");
                // The diff carries ids; resolve back to the canonical name the
                // JVM dump is written in. A position absent from the diff is
                // unchanged, i.e. the pre-surface block.
                let got_id = result
                    .get(&key(x, y, z))
                    .copied()
                    .unwrap_or_else(|| pre_fn(x, y, z).state);
                let got = interner.name_of(got_id);
                if want == got {
                    matching += 1;
                } else if first_divergence.is_none() {
                    first_divergence = Some((x, y, z, want.clone(), got.to_string()));
                }
            }
        }
    }

    let pct = 100.0 * matching as f64 / total as f64;
    println!(
        "surface-rule whole-chunk parity [{label}] chunk ({},{}): {matching}/{total} = {pct:.4}% bit-exact",
        r.chunk_x, r.chunk_z
    );

    if let Some((x, y, z, want, got)) = first_divergence {
        let bx = r.chunk_x * 16 + x;
        let bz = r.chunk_z * 16 + z;
        panic!(
            "surface divergence [{label}] at local {x},{y},{z} (world {bx},{y},{bz}): \
             jvm={want} rust={got} ({matching}/{total} = {pct:.4}%)"
        );
    }
    assert_eq!(matching, total);
}

#[test]
fn surface_rules_match_jvm_ocean_chunk() {
    run_fixture("ocean", include_str!("support/surface_plains_jvm.txt"));
}

#[test]
fn surface_rules_match_jvm_land_chunk() {
    run_fixture("land", include_str!("support/surface_plains_land_jvm.txt"));
}
