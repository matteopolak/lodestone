//! Block-for-block parity of the **interpolated** `final_density` field over a
//! whole chunk, reported as a percentage.
//!
//! `region_parity` proves the raw noise router at `SinglePointContext`. This
//! test proves the next stage: vanilla's `NoiseChunk` does **not** write the
//! point field to blocks — it samples `final_density` at 4×8×4 cell corners and
//! trilinearly interpolates (plus `flat_cache` XZ/quart snapping). The oracle
//! `scripts/worldgen-oracle/DensityChunkOracle.java` drives the real 26.2
//! `NoiseChunk` interpolation loop (the exact `NoiseBasedChunkGenerator.doFill`
//! order) and dumps `getInterpolatedDensity()` for every block in chunk (0,0) at
//! seed 42 — 16×16×384 = 98304 blocks. [`NoiseChunkSampler`] must reproduce it
//! bit-for-bit.
//!
//! Scope (honest): this proves cell interpolation + flat-cache snapping of the
//! `final_density` field. It is *not* surface rules, aquifers, or carvers — the
//! block field here is the pre-aquifer noise density, not final block states.

use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{Builder, NoiseChunkSampler, NoiseParams, Resolver};
use serde_json::Value;

const REFERENCE: &str = include_str!("support/density_chunk_jvm.txt");
const SEED: i64 = 42;
const CELL_WIDTH: i32 = 4;
const CELL_HEIGHT: i32 = 8;

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

fn bits(v: f64) -> String {
    format!("{:x}", v.to_bits())
}

#[test]
fn interpolated_final_density_matches_jvm_over_whole_chunk() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();

    let builder = Builder::new(SEED, &resolver);
    let final_density = builder.build(&settings["noise_router"]["final_density"]);
    let sampler =
        NoiseChunkSampler::new(final_density, builder.slot_count(), CELL_WIDTH, CELL_HEIGHT);

    let mut mismatches = Vec::new();
    let mut total = 0usize;
    for line in REFERENCE.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        total += 1;
        let (coords, exp) = line.rsplit_once(' ').expect("malformed line");
        let mut it = coords.split(',');
        let x: i32 = it.next().unwrap().parse().unwrap();
        let y: i32 = it.next().unwrap().parse().unwrap();
        let z: i32 = it.next().unwrap().parse().unwrap();
        let got = bits(sampler.final_density(x, y, z));
        if got != exp {
            mismatches.push(format!("{x},{y},{z}: rust={got} jvm={exp}"));
        }
    }

    let matched = total - mismatches.len();
    let pct = 100.0 * matched as f64 / total as f64;
    println!(
        "interpolated final-density whole-chunk parity: {matched}/{total} = {pct:.4}% bit-exact"
    );

    if !mismatches.is_empty() {
        let shown = mismatches.len().min(40);
        panic!(
            "{}/{} blocks diverged ({:.4}% match):\n{}",
            mismatches.len(),
            total,
            pct,
            mismatches[..shown].join("\n")
        );
    }
}
