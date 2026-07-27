//! Block-for-block parity of the noise router over a **whole contiguous chunk
//! region**, reported as a percentage.
//!
//! Where `density_parity` samples scattered points across four seeds,
//! `scripts/worldgen-oracle/RegionOracle.java` dumps the router's `final_density`
//! and `depth` for every block in a full 16×16 chunk footprint across a
//! contiguous 64-block vertical band (plus the five climate channels over the
//! 16×16 surface) at seed 42, using the real 26.2 registries. This test rebuilds
//! the same region with the Rust interpreter and scores it block-for-block,
//! matching the project's "whole-corpus coverage over spot checks" discipline
//! (plan §12).
//!
//! Scope (honest): this is the **noise-router terrain-shape stage** at vanilla's
//! `SinglePointContext` — it is *not* the interpolated per-block sampling
//! `NoiseChunk` performs, nor surface rules / aquifers / carvers. It proves the
//! router math over a whole region, which is the foundation those stages sit on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{Builder, Context, NoiseParams, Resolver};
use serde_json::Value;

const REFERENCE: &str = include_str!("support/region_jvm.txt");

const Y_LO: i32 = -32;
const Y_HI: i32 = 32;
const SEED: i64 = 42;

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

fn reference() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in REFERENCE.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line.rsplit_once(' ').expect("malformed line");
        map.insert(k.to_string(), v.to_string());
    }
    map
}

fn bits(v: f64) -> String {
    format!("{:x}", v.to_bits())
}

#[test]
fn noise_router_matches_jvm_over_whole_region() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let router = &settings["noise_router"];

    let builder = Builder::new(SEED, &resolver);
    let continents = builder.build(&router["continents"]);
    let erosion = builder.build(&router["erosion"]);
    let ridges = builder.build(&router["ridges"]);
    let temperature = builder.build(&router["temperature"]);
    let vegetation = builder.build(&router["vegetation"]);
    let depth = builder.build(&router["depth"]);
    let final_density = builder.build(&router["final_density"]);

    let mut got: BTreeMap<String, String> = BTreeMap::new();
    for x in 0..16 {
        for z in 0..16 {
            got.insert(
                format!("continents.{x},{z}"),
                bits(continents.compute(Context::new(x, 0, z))),
            );
            got.insert(
                format!("erosion.{x},{z}"),
                bits(erosion.compute(Context::new(x, 0, z))),
            );
            got.insert(
                format!("ridges.{x},{z}"),
                bits(ridges.compute(Context::new(x, 0, z))),
            );
            got.insert(
                format!("temperature.{x},{z}"),
                bits(temperature.compute(Context::new(x, 0, z))),
            );
            got.insert(
                format!("vegetation.{x},{z}"),
                bits(vegetation.compute(Context::new(x, 0, z))),
            );
        }
    }
    for x in 0..16 {
        for y in Y_LO..Y_HI {
            for z in 0..16 {
                let ctx = Context::new(x, y, z);
                got.insert(format!("depth.{x},{y},{z}"), bits(depth.compute(ctx)));
                got.insert(format!("fd.{x},{y},{z}"), bits(final_density.compute(ctx)));
            }
        }
    }

    let expected = reference();
    assert_eq!(
        got.len(),
        expected.len(),
        "probe count mismatch: rust={} jvm={}",
        got.len(),
        expected.len()
    );

    let mut mismatches = Vec::new();
    for (k, exp) in &expected {
        match got.get(k) {
            Some(actual) if actual == exp => {}
            Some(actual) => mismatches.push(format!("{k}: rust={actual} jvm={exp}")),
            None => mismatches.push(format!("{k}: MISSING")),
        }
    }

    let total = expected.len();
    let matched = total - mismatches.len();
    let pct = 100.0 * matched as f64 / total as f64;
    println!("noise-router whole-region parity: {matched}/{total} = {pct:.4}% bit-exact");

    if !mismatches.is_empty() {
        let shown = mismatches.len().min(40);
        panic!(
            "{}/{} region probes diverged ({:.4}% match):\n{}",
            mismatches.len(),
            total,
            pct,
            mismatches[..shown].join("\n")
        );
    }
}
