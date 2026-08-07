//! Block-for-block parity of the **carvers** (caves + canyons) over whole chunks.
//!
//! `surface_parity` proves the post-surface column; this test proves the next
//! stage: vanilla's `NoiseBasedChunkGenerator.applyCarvers` scans the 17×17
//! source-chunk neighbourhood, seeds a positional RNG per source chunk × carver,
//! rolls the `isStartChunk` probability, and — on success — carves tunnels and
//! ravines that overwrite the centre chunk's blocks with air/water/lava.
//!
//! The oracle `scripts/worldgen-oracle/CarverOracle.java` drives the real 26.2
//! `doFill` + `buildSurface` + `applyCarvers` at seed 42, biome pinned to plains
//! (`FixedBiomeSource`), and — critically — binds the vanilla block tags so
//! `#overworld_carver_replaceables` is populated (without it, `canReplaceBlock`
//! is always false and nothing carves). It dumps the post-carve column
//! (`carve.*`), plus two probes per source chunk × carver: `start.*` (did the
//! carver fire) and `probe.*` (the outer RNG's `nextLong()` *after* the carver —
//! a single i64 that diverges the instant a carver consumes a different *number*
//! of draws than vanilla, which is the failure mode that silently desynchronises
//! everything placed afterwards).
//!
//! The carve **input** is the matching `surface_*_jvm.txt` fixture's `post.*`
//! column (reused by name; the chunk coords are asserted to agree). Two
//! fixtures, both `minecraft:plains`:
//!   * ocean chunk (0,0)      — carves ~3060 blocks (caves reaching the seabed,
//!     water substance from the aquifer).
//!   * land chunk (-120,-120) — carves ~472 blocks (fewer caves intersect the
//!     centre column). Testing both guards the aquifer's air-vs-water branch and
//!     the sea-level-dependent carve substance.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use lodestone_worldgen::aquifer::AquiferSystem;
use lodestone_worldgen::carver::{CarveGrid, CarveObserver, CarverConfig, apply_carvers};
use lodestone_worldgen::density::{Builder, NoiseParams, Resolver};
use lodestone_worldgen::rng::RandomSource;
use lodestone_worldgen::surface::{BlockCanon, SurfaceSystem};
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

/// Recursively resolve a block tag's closure into a set of base block names
/// (e.g. `minecraft:deepslate`). Sub-tag references (`#minecraft:...`) recurse;
/// plain ids are added directly. Object entries (`{"id": ..., "required": ...}`)
/// are handled too.
fn resolve_block_tag(root: &Path, id: &str, out: &mut HashSet<String>, seen: &mut HashSet<String>) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let name = id.strip_prefix("minecraft:").unwrap_or(id);
    let path = root.join("tags/block").join(format!("{name}.json"));
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading block tag {}: {e}", path.display()));
    let doc: Value = serde_json::from_str(&text).unwrap();
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

struct Reference {
    carve: HashMap<(i32, i32, i32), String>,
    starts: HashMap<(i32, i32, usize), bool>,
    probes: HashMap<(i32, i32, usize), i64>,
    chunk_x: i32,
    chunk_z: i32,
    changed: usize,
}

fn parse_carver_reference(text: &str) -> Reference {
    let mut r = Reference {
        carve: HashMap::new(),
        starts: HashMap::new(),
        probes: HashMap::new(),
        chunk_x: 0,
        chunk_z: 0,
        changed: 0,
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("carve.") {
            let (x, y, z) = parse_xyz(coords);
            r.carve.insert((x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("start.") {
            let (dx, dz, i) = parse_xyz(coords);
            r.starts.insert((dx, dz, i as usize), rest == "1");
        } else if let Some(coords) = tag.strip_prefix("probe.") {
            let (dx, dz, i) = parse_xyz(coords);
            r.probes.insert((dx, dz, i as usize), rest.parse().unwrap());
        } else if tag == "meta.chunkX" {
            r.chunk_x = rest.parse().unwrap();
        } else if tag == "meta.chunkZ" {
            r.chunk_z = rest.parse().unwrap();
        } else if tag == "meta.changed" {
            r.changed = rest.parse().unwrap();
        }
    }
    r
}

/// Parsed pieces of the surface fixture that the carver run consumes: the
/// post-surface column (carve input), the chunk coords, the `WORLD_SURFACE_WG`
/// heightmap (only the `steep` surface condition reads it), the biome, and the
/// canonical-state map (both needed to rebuild the surface rule for `topMaterial`).
struct SurfaceInput {
    post: HashMap<(i32, i32, i32), String>,
    cx: i32,
    cz: i32,
    hm: HashMap<(i32, i32), i32>,
    biome: String,
    canon: BlockCanon,
}

fn parse_surface_post(text: &str) -> SurfaceInput {
    let mut post = HashMap::new();
    let mut hm = HashMap::new();
    let mut canon = BlockCanon::default();
    let mut biome = String::new();
    let mut cx = 0;
    let mut cz = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (tag, rest) = line.split_once(' ').expect("tag value");
        if let Some(coords) = tag.strip_prefix("post.") {
            let (x, y, z) = parse_xyz(coords);
            post.insert((x, y, z), rest.to_string());
        } else if let Some(coords) = tag.strip_prefix("hm.") {
            let (x, z) = coords.split_once(',').expect("hm x,z");
            hm.insert(
                (x.parse().unwrap(), z.parse().unwrap()),
                rest.parse().unwrap(),
            );
        } else if let Some(part_key) = tag.strip_prefix("canonmap.") {
            canon.insert(part_key.to_string(), rest.to_string());
        } else if tag == "meta.biome" {
            biome = rest.to_string();
        } else if tag == "meta.chunkX" {
            cx = rest.parse().unwrap();
        } else if tag == "meta.chunkZ" {
            cz = rest.parse().unwrap();
        }
    }
    SurfaceInput {
        post,
        cx,
        cz,
        hm,
        biome,
        canon,
    }
}

fn parse_xyz(s: &str) -> (i32, i32, i32) {
    let mut it = s.split(',');
    let x = it.next().unwrap().parse().unwrap();
    let y = it.next().unwrap().parse().unwrap();
    let z = it.next().unwrap().parse().unwrap();
    (x, y, z)
}

/// Records each carver's `isStartChunk` result and the outer RNG's `nextLong()`
/// draw-count probe, keyed by source-chunk offset from the centre chunk.
struct Recorder {
    center_x: i32,
    center_z: i32,
    starts: HashMap<(i32, i32, usize), bool>,
    probes: HashMap<(i32, i32, usize), i64>,
    started_count: usize,
}

impl CarveObserver for Recorder {
    fn after_carver<R: RandomSource>(
        &mut self,
        source_x: i32,
        source_z: i32,
        index: usize,
        started: bool,
        random: &mut R,
    ) {
        let dx = source_x - self.center_x;
        let dz = source_z - self.center_z;
        self.starts.insert((dx, dz, index), started);
        if started {
            self.started_count += 1;
        }
        // The probe resets on the next carver's setLargeFeatureSeed, so this draw
        // is non-destructive — it mirrors the oracle exactly.
        self.probes.insert((dx, dz, index), random.next_long());
    }
}

fn run_fixture(label: &str, surface_text: &str, carver_text: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();

    let si = parse_surface_post(surface_text);
    let post = &si.post;
    let r = parse_carver_reference(carver_text);
    assert_eq!(
        (si.cx, si.cz),
        (r.chunk_x, r.chunk_z),
        "[{label}] surface and carver fixtures must be the same chunk"
    );
    assert_eq!(si.biome, "minecraft:plains", "[{label}] fixture biome");

    // Resolve the replaceable tag closure from disk JSON.
    let mut replaceable = HashSet::new();
    let mut seen = HashSet::new();
    resolve_block_tag(
        &root,
        "minecraft:overworld_carver_replaceables",
        &mut replaceable,
        &mut seen,
    );
    assert!(
        replaceable.contains("minecraft:stone") && replaceable.contains("minecraft:water"),
        "[{label}] replaceable tag closure looks wrong: {} entries",
        replaceable.len()
    );

    // Parse the three plains carvers, in the biome's carver order.
    let carvers: Vec<CarverConfig> = ["cave", "cave_extra_underground", "canyon"]
        .iter()
        .map(|name| {
            let doc: Value = serde_json::from_str(
                &std::fs::read_to_string(
                    root.join("configured_carver").join(format!("{name}.json")),
                )
                .unwrap(),
            )
            .unwrap();
            CarverConfig::parse(&doc)
        })
        .collect();

    let builder = Builder::new(SEED, &resolver);
    let aquifer = AquiferSystem::new(&settings, &builder, r.chunk_x, r.chunk_z);

    // Rebuild the surface rule so carvers can re-cap dirt exposed beneath a
    // carved grass block via `topMaterial`. plains is not cold enough to snow.
    // U21: `SurfaceSystem` interns its result states, so it takes the table.
    // `top_material` still returns `Option<String>` — the carver seam was
    // deliberately left on strings — so nothing else here changes.
    let interner = std::sync::Arc::new(lodestone_worldgen::interner::StateInterner::new());
    let surface = SurfaceSystem::new(&settings, &builder, &si.canon, &interner);
    let hm_fn = |x: i32, z: i32| -> i32 { *si.hm.get(&(x, z)).expect("heightmap") };
    let top_material = |x: i32, y: i32, z: i32, under_fluid: bool| -> Option<String> {
        surface.top_material(x, y, z, under_fluid, &hm_fn, &si.biome, false)
    };

    // Build the world-keyed input grid from the post-surface column.
    let origin_x = r.chunk_x * 16;
    let origin_z = r.chunk_z * 16;
    let mut world_blocks = HashMap::new();
    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..(MIN_Y + HEIGHT) {
                let block = post.get(&(x, y, z)).expect("post block");
                world_blocks.insert((origin_x + x, y, origin_z + z), block.clone());
            }
        }
    }
    let mut grid = CarveGrid::new(world_blocks);

    let mut recorder = Recorder {
        center_x: r.chunk_x,
        center_z: r.chunk_z,
        starts: HashMap::new(),
        probes: HashMap::new(),
        started_count: 0,
    };

    let carvers_for_source = |_source_x: i32, _source_z: i32| carvers.clone();
    apply_carvers(
        SEED,
        r.chunk_x,
        r.chunk_z,
        MIN_Y,
        HEIGHT,
        &carvers_for_source,
        &mut grid,
        &aquifer,
        &replaceable,
        &top_material,
        &mut recorder,
    );

    // --- Draw-count / start-gate parity (element-wise, name the divergence) ---
    assert_eq!(
        recorder.starts.len(),
        r.starts.len(),
        "[{label}] start count mismatch"
    );
    let mut started_total = 0usize;
    for (k, &want) in &r.starts {
        let got = *recorder.starts.get(k).expect("recorded start");
        if want {
            started_total += 1;
        }
        assert_eq!(
            got, want,
            "[{label}] isStartChunk divergence at source offset {},{} carver {}: jvm={want} rust={got}",
            k.0, k.1, k.2
        );
    }
    for (k, &want) in &r.probes {
        let got = *recorder.probes.get(k).expect("recorded probe");
        assert_eq!(
            got, want,
            "[{label}] draw-count probe divergence at source offset {},{} carver {}: \
             jvm nextLong={want} rust nextLong={got} (a carver consumed a different number of RNG draws)",
            k.0, k.1, k.2
        );
    }
    assert!(
        recorder.started_count > 0,
        "[{label}] no carver started — vacuous probe check"
    );
    assert_eq!(
        recorder.started_count, started_total,
        "[{label}] started-count bookkeeping mismatch"
    );

    // --- Whole-chunk block parity (element-wise, name the divergent block) ---
    let result = grid.into_blocks();
    let mut total = 0usize;
    let mut matching = 0usize;
    let mut changed = 0usize;
    let mut first_divergence: Option<(i32, i32, i32, String, String)> = None;

    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..(MIN_Y + HEIGHT) {
                total += 1;
                let want = r.carve.get(&(x, y, z)).expect("carve block");
                let got = result
                    .get(&(origin_x + x, y, origin_z + z))
                    .expect("result block");
                let input = post.get(&(x, y, z)).expect("post block");
                if want != input {
                    changed += 1;
                }
                if want == got {
                    matching += 1;
                } else if first_divergence.is_none() {
                    first_divergence = Some((x, y, z, want.clone(), got.clone()));
                }
            }
        }
    }

    let pct = 100.0 * matching as f64 / total as f64;
    println!(
        "carver whole-chunk parity [{label}] chunk ({},{}): {matching}/{total} = {pct:.4}% bit-exact \
         ({changed} blocks carved, {} carvers fired)",
        r.chunk_x, r.chunk_z, recorder.started_count
    );

    if let Some((x, y, z, want, got)) = first_divergence {
        let bx = origin_x + x;
        let bz = origin_z + z;
        let input = post.get(&(x, y, z)).cloned().unwrap_or_default();
        panic!(
            "carver divergence [{label}] at local {x},{y},{z} (world {bx},{y},{bz}): \
             input={input} jvm={want} rust={got} ({matching}/{total} = {pct:.4}%)"
        );
    }
    assert_eq!(matching, total);

    // Anti-vacuity: the fixture's own change count must agree, and it must be a
    // meaningful number of blocks — otherwise we would be comparing an
    // uncarved column to itself and calling it agreement.
    assert_eq!(
        changed, r.changed,
        "[{label}] carved-block count vs fixture meta"
    );
    assert!(
        changed > 200,
        "[{label}] only {changed} blocks carved — suspiciously few, check the carve path"
    );
}

#[test]
fn carvers_match_jvm_ocean_chunk() {
    run_fixture(
        "ocean",
        include_str!("support/surface_plains_jvm.txt"),
        include_str!("support/carver_plains_jvm.txt"),
    );
}

#[test]
fn carvers_match_jvm_land_chunk() {
    run_fixture(
        "land",
        include_str!("support/surface_plains_land_jvm.txt"),
        include_str!("support/carver_plains_land_jvm.txt"),
    );
}
