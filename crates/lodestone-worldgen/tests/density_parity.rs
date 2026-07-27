//! Bit-exact parity of the density-function / noise-router interpreter against a
//! real JVM.
//!
//! `scripts/worldgen-oracle/DensityOracle.java` bootstraps the **actual 26.2
//! vanilla registries**, builds `RandomState.create(provider, OVERWORLD, seed)`,
//! and dumps `NoiseRouter` channel outputs at fixed block positions via
//! `DensityFunction.SinglePointContext`; the dump is checked in as
//! `support/density_jvm.txt`. This test loads the same data-driven noise router
//! from the checked-in vanilla JSON, builds it with the Rust interpreter, and
//! evaluates the same probes, diffing element-wise by raw bit pattern and naming
//! the exact key that diverges (plan §12.6 forbids hash-only comparison).
//!
//! Because the oracle uses `SinglePointContext` (no cell interpolation), passing
//! this test proves the **interpreter tree math** — every node type, the noise
//! seeding, and the f32 spline arithmetic — not the interpolated/cached chunk
//! sampling that `NoiseChunk` layers on top (a separate, later stage).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use lodestone_worldgen::density::{Builder, Context, NoiseParams, Resolver};
use serde_json::Value;

const REFERENCE: &str = include_str!("support/density_jvm.txt");

/// Sample coordinates — must match `DensityOracle` exactly and in order.
const XS: [i32; 8] = [0, 1, 4, 7, 16, -13, 100, -400];
const YS: [i32; 9] = [-64, -32, 0, 40, 63, 80, 120, 200, 319];
const ZS: [i32; 5] = [0, 5, -20, 37, 200];
const SEEDS: [i64; 4] = [0, 42, 1_234_567_890_123, -8_823_894_646];

/// The router channels evaluated across x/z at y=0 (2D climate channels).
const CLIMATE: [(&str, &str); 5] = [
    ("continents", "continents"),
    ("erosion", "erosion"),
    ("ridges", "ridges"),
    ("temperature", "temperature"),
    ("vegetation", "vegetation"),
];

/// The router channels evaluated across the full 3D grid.
const VOLUMETRIC: [(&str, &str); 3] = [
    ("depth", "depth"),
    ("finalDensity", "final_density"),
    ("barrier", "barrier"),
];

/// Filesystem-backed resolver over the checked-in vanilla worldgen JSON.
///
/// In production this data lives in the version crate as generated data (plan
/// §3); here it is staged as test fixtures because version crates are owned by
/// other agents this session.
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
        let first_octave = v["firstOctave"].as_i64().expect("firstOctave") as i32;
        let amplitudes = v["amplitudes"]
            .as_array()
            .expect("amplitudes")
            .iter()
            .map(|a| a.as_f64().expect("amplitude"))
            .collect();
        NoiseParams {
            first_octave,
            amplitudes,
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
fn density_router_matches_jvm() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/worldgen_data");
    let resolver = FsResolver { root: root.clone() };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let router = &settings["noise_router"];

    let expected = reference();
    let mut got: BTreeMap<String, String> = BTreeMap::new();

    for seed in SEEDS {
        let builder = Builder::new(seed, &resolver);
        let tag = format!("ow[{seed}]");

        for (label, key) in CLIMATE {
            let df = builder.build(&router[key]);
            for x in XS {
                for z in ZS {
                    let v = df.compute(Context::new(x, 0, z));
                    got.insert(format!("{tag}.{label}.{x},{z}"), bits(v));
                }
            }
        }

        for (label, key) in VOLUMETRIC {
            let df = builder.build(&router[key]);
            for x in XS {
                for y in YS {
                    for z in ZS {
                        let v = df.compute(Context::new(x, y, z));
                        got.insert(format!("{tag}.{label}.{x},{y},{z}"), bits(v));
                    }
                }
            }
        }
    }

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
            None => mismatches.push(format!("{k}: MISSING in rust output")),
        }
    }

    if !mismatches.is_empty() {
        let shown = mismatches.len().min(40);
        panic!(
            "{} / {} density probes diverged from the JVM:\n{}",
            mismatches.len(),
            expected.len(),
            mismatches[..shown].join("\n")
        );
    }
}
