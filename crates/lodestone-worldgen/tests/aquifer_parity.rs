//! Block-for-block parity of the **aquifer + density fill** (vanilla's own fill step) over whole
//! chunks.
//!
//! `chunk_parity` proves the interpolated `final_density` scalar field;
//! `surface_parity` proves the surface rules *on top of* an aquifer-filled
//! column. This test proves the stage *between* them: vanilla's own
//! chunk-generator fill step asks its own aquifer for every block whether
//! it is the default block (stone), a fluid (water/lava) or air, building local
//! water tables and air pockets from the barrier/floodedness/spread/lava noise
//! routes and the positional aquifer-centre RNG. That decision is what the
//! surface system consumes, so it must be bit-exact first.
//!
//! The oracle is the very same `scripts/worldgen-oracle/SurfaceOracle.java` used
//! by `surface_parity`: its `pre.*` column is the real 26.2 fill-step output
//! (aquifer applied), dumped block-for-block at seed 42 with the biome pinned.
//! [`AquiferSystem`] must reproduce that column exactly — 16×16×384 = 98304
//! blocks per chunk.
//!
//! Two fixtures, both `minecraft:plains`:
//!   * `surface_plains_jvm.txt`      — chunk (0,0), oceanic: exercises the sea
//!     water table (`minecraft:water[level=0]`) and air above it.
//!   * `surface_plains_land_jvm.txt` — chunk (-120,-120), land: exercises deep
//!     lava pockets (`minecraft:lava[level=0]`) and the below-min-y sentinel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lodestone_worldgen::aquifer::{AquiferSystem, BlockKind};
use lodestone_worldgen::density::{Builder, NoiseParams, Resolver};
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

struct Reference {
    pre: HashMap<(i32, i32, i32), String>,
    biome: String,
    chunk_x: i32,
    chunk_z: i32,
}

fn parse_reference(text: &str) -> Reference {
    let mut r = Reference {
        pre: HashMap::new(),
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
            r.pre.insert((x, y, z), rest.to_string());
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

fn block_to_string(kind: BlockKind) -> &'static str {
    match kind {
        BlockKind::Stone => "minecraft:stone",
        BlockKind::Air => "minecraft:air",
        BlockKind::Water => "minecraft:water[level=0]",
        BlockKind::Lava => "minecraft:lava[level=0]",
    }
}

fn run_fixture(label: &str, text: &str, expect_water: bool, expect_lava: bool) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();

    let r = parse_reference(text);
    assert_eq!(r.biome, "minecraft:plains", "fixture biome");

    let builder = Builder::new(SEED, &resolver);
    let aquifer = AquiferSystem::new(&settings, &builder, r.chunk_x, r.chunk_z);

    let origin_x = r.chunk_x * 16;
    let origin_z = r.chunk_z * 16;

    let mut total = 0usize;
    let mut matching = 0usize;
    let mut stone = 0usize;
    let mut water = 0usize;
    let mut lava = 0usize;
    let mut air = 0usize;
    let mut first_divergence: Option<(i32, i32, i32, String, String)> = None;

    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..(MIN_Y + HEIGHT) {
                total += 1;
                let want = r.pre.get(&(x, y, z)).expect("pre block");
                let kind = aquifer.block_at(origin_x + x, y, origin_z + z);
                match kind {
                    BlockKind::Stone => stone += 1,
                    BlockKind::Water => water += 1,
                    BlockKind::Lava => lava += 1,
                    BlockKind::Air => air += 1,
                }
                let got = block_to_string(kind);
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
        "aquifer/fill-step whole-chunk parity [{label}] chunk ({},{}): {matching}/{total} = {pct:.4}% bit-exact",
        r.chunk_x, r.chunk_z
    );

    if let Some((x, y, z, want, got)) = first_divergence {
        let bx = origin_x + x;
        let bz = origin_z + z;
        panic!(
            "aquifer divergence [{label}] at local {x},{y},{z} (world {bx},{y},{bz}): \
             jvm={want} rust={got} ({matching}/{total} = {pct:.4}%)"
        );
    }

    // Anti-vacuity: the loop must have covered the full column, and the fill must
    // have produced a real terrain (not all air), including the fluid the fixture
    // is chosen to exercise — so this cannot pass by comparing empty air to air.
    assert_eq!(total, 16 * 16 * HEIGHT as usize, "column not fully scanned");
    assert!(stone > 0, "[{label}] no solid blocks — vacuous fill");
    assert!(air > 0, "[{label}] no air — vacuous fill");
    if expect_water {
        assert!(water > 0, "[{label}] expected a water table, found none");
    }
    if expect_lava {
        assert!(lava > 0, "[{label}] expected lava pockets, found none");
    }
    assert_eq!(matching, total);
}

#[test]
fn aquifer_matches_jvm_ocean_chunk() {
    run_fixture(
        "ocean",
        include_str!("support/surface_plains_jvm.txt"),
        true,
        false,
    );
}

#[test]
fn aquifer_matches_jvm_land_chunk() {
    run_fixture(
        "land",
        include_str!("support/surface_plains_land_jvm.txt"),
        false,
        true,
    );
}
