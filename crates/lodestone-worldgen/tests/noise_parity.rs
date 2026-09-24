//! Bit-exact parity of the Perlin/normal noise stack against a real JVM.
//!
//! `scripts/worldgen-oracle/NoiseOracle.java` calls the **actual 26.2 game
//! classes** (`ImprovedNoise`, `PerlinNoise`, `NormalNoise`) and dumps their
//! outputs; the dump is checked in as `support/noise_jvm.txt`. This test rebuilds
//! every probe with the Rust implementation and diffs element-wise, naming the
//! exact key that diverges (plan §12.6 forbids hash-only comparison).
//!
//! Doubles are compared by raw bit pattern, so equality is exact. Unlike the RNG
//! suite there is no gaussian tolerance here: the noise path uses only
//! `nextDouble`/`nextInt` and IEEE arithmetic, so every probe must be bit-exact.

use std::collections::BTreeMap;

use lodestone_worldgen::{
    ImprovedNoise, LegacyRandomSource, NormalNoise, PerlinNoise, RandomSource,
    XoroshiroRandomSource,
};

const REFERENCE: &str = include_str!("support/noise_jvm.txt");

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

#[derive(Default)]
struct Dump {
    got: BTreeMap<String, String>,
}

impl Dump {
    fn d(&mut self, k: impl Into<String>, v: f64) {
        self.got.insert(k.into(), format!("{:x}", v.to_bits()));
    }
}

const SEEDS: [i64; 4] = [0, 42, 1_234_567_890_123, -8_823_894_646];

/// Sample coordinates — must match `NoiseOracle.PTS` exactly and in order.
const PTS: [[f64; 3]; 10] = [
    [0.0, 0.0, 0.0],
    [1.5, 2.5, 3.5],
    [-4.25, 10.0, -7.75],
    [123.456, -64.0, 987.654],
    [0.1, 0.2, 0.3],
    [-1000.5, 32.0, 2000.25],
    [33_554_431.0, 0.0, -33_554_433.0],
    [16.0, -48.0, 16.0],
    [-0.5, -0.5, -0.5],
    [50_000.5, 128.0, -50_000.5],
];

/// Parameter sets — must match `NoiseOracle.params` exactly and in order.
fn params() -> Vec<(&'static str, i32, Vec<f64>)> {
    vec![
        ("temperature", -10, vec![1.5, 0.0, 1.0, 0.0, 0.0, 0.0]),
        ("vegetation", -8, vec![1.0, 1.0, 0.0, 0.0, 0.0, 0.0]),
        (
            "continentalness",
            -9,
            vec![1.0, 1.0, 2.0, 2.0, 2.0, 1.0, 1.0, 1.0, 1.0],
        ),
        ("erosion", -9, vec![1.0, 1.0, 0.0, 1.0, 1.0]),
        ("ridge", -7, vec![1.0, 2.0, 1.0, 0.0, 0.0, 0.0]),
        ("single", -6, vec![1.0]),
        ("aquifer_barrier", -3, vec![1.0]),
    ]
}

#[test]
fn noise_matches_jvm_bit_for_bit() {
    let mut d = Dump::default();

    for (label, xoro) in [("xoro", true), ("legacy", false)] {
        for &seed in &SEEDS {
            // ImprovedNoise
            let t = format!("improved[{label},{seed}]");
            let (xo, yo, zo, samples) = if xoro {
                improved_probe(&mut XoroshiroRandomSource::new(seed))
            } else {
                improved_probe(&mut LegacyRandomSource::new(seed))
            };
            d.d(format!("{t}.xo"), xo);
            d.d(format!("{t}.yo"), yo);
            d.d(format!("{t}.zo"), zo);
            for (i, v) in samples.iter().enumerate() {
                d.d(format!("{t}.noise.{i}"), *v);
            }

            // PerlinNoise
            for (name, first_octave, amps) in params() {
                let t = format!("perlin[{label},{seed},{name}]");
                let vals = if xoro {
                    perlin_probe(&mut XoroshiroRandomSource::new(seed), first_octave, &amps)
                } else {
                    perlin_probe(&mut LegacyRandomSource::new(seed), first_octave, &amps)
                };
                for (i, v) in vals.iter().enumerate() {
                    d.d(format!("{t}.val.{i}"), *v);
                }
            }

            // NormalNoise
            for (name, first_octave, amps) in params() {
                let t = format!("normal[{label},{seed},{name}]");
                let vals = if xoro {
                    normal_probe(&mut XoroshiroRandomSource::new(seed), first_octave, &amps)
                } else {
                    normal_probe(&mut LegacyRandomSource::new(seed), first_octave, &amps)
                };
                for (i, v) in vals.iter().enumerate() {
                    d.d(format!("{t}.val.{i}"), *v);
                }
            }
        }
    }

    let reference = reference();
    let mut mismatches = Vec::new();
    let mut checked = 0usize;
    for (key, expected) in &reference {
        match d.got.get(key) {
            None => mismatches.push(format!("  {key}: MISSING on Rust side (java={expected})")),
            Some(actual) if actual == expected => checked += 1,
            Some(actual) => mismatches.push(format!("  {key}: rust={actual} java={expected}")),
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} of {} noise probes diverged from the JVM:\n{}",
        mismatches.len(),
        reference.len(),
        mismatches.join("\n")
    );
    assert_eq!(checked, reference.len());
    assert!(checked > 1000, "suspiciously few probes checked: {checked}");
    eprintln!(
        "noise parity: {checked}/{} probes bit-exact against the JVM",
        reference.len()
    );
}

fn improved_probe<R: RandomSource>(r: &mut R) -> (f64, f64, f64, Vec<f64>) {
    let n = ImprovedNoise::new(r);
    let samples = PTS.iter().map(|p| n.noise(p[0], p[1], p[2])).collect();
    (n.xo, n.yo, n.zo, samples)
}

fn perlin_probe<R: RandomSource>(r: &mut R, first_octave: i32, amps: &[f64]) -> Vec<f64> {
    let n = PerlinNoise::create(r, first_octave, amps);
    PTS.iter().map(|p| n.get_value(p[0], p[1], p[2])).collect()
}

fn normal_probe<R: RandomSource>(r: &mut R, first_octave: i32, amps: &[f64]) -> Vec<f64> {
    let n = NormalNoise::create(r, first_octave, amps);
    PTS.iter().map(|p| n.get_value(p[0], p[1], p[2])).collect()
}
