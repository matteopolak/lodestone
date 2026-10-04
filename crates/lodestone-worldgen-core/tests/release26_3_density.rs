//! The 26.3 engine against hashes printed by the real server's own density
//! functions (`scripts/worldgen-oracle-26-3`, `DensityOracle263`).
//!
//! Each fixture line is `fn <settings> <seed> <name> scalar|vol<k> <fnv> <n> <first>`.
//! The scalar grid and the volume shapes mirror the oracle's tables; every sample
//! is hashed by raw `f32` bits, so a one-ulp difference fails.

use std::collections::BTreeMap;

use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_core::engine::release26_3::volume::Volume;
use lodestone_worldgen_data_26_3 as data;

const XS: [i32; 10] = [0, 1, 4, 7, 16, -13, 100, -400, 5000, -77777];
const YS: [i32; 9] = [-64, -33, 0, 40, 63, 80, 120, 200, 319];
const ZS: [i32; 6] = [0, 5, -20, 37, 200, 12345];
const SHAPES: [[i32; 9]; 7] = [
    [3, 5, 3, -5, -3, 7, 1, 1, 1],
    [5, 13, 5, -16, -64, 32, 4, 8, 4],
    [1, 7, 1, 33, 10, -9, 1, 1, 1],
    [5, 1, 5, -20, 0, -20, 4, 1, 4],
    [16, 24, 16, 0, -64, 0, 1, 1, 1],
    [4, 5, 3, 3, -17, 5, 8, 4, 4],
    [17, 49, 17, 1_000_000, -64, -64000, 1, 1, 1],
];

fn fnv(values: &[f32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in values {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

fn check(settings: &str, seed: i64, fixture: &str) {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS);
    let mut want: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut extra = Vec::new();
    for line in fixture.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        if let Some(path) = f[3].strip_prefix("fn.") {
            if !extra.contains(&path) {
                extra.push(path);
            }
        }
        want.insert((f[3].to_owned(), f[4].to_owned()), f[5..].join(" "));
    }
    let (gen_, ids) = TerrainGenerator::load_with(&res, settings, seed, &extra).expect("loads");
    let r = gen_.router;
    let mut named: Vec<(String, u32)> = vec![
        ("router.temperature".into(), r.temperature),
        ("router.vegetation".into(), r.vegetation),
        ("router.continents".into(), r.continents),
        ("router.erosion".into(), r.erosion),
        ("router.depth".into(), r.depth),
        ("router.ridges".into(), r.ridges),
        ("router.chunk_surface_level".into(), r.chunk_surface_level),
        ("router.final_density".into(), r.final_density),
    ];
    if let Some(a) = gen_.aquifer {
        named.push(("aquifer.barrier".into(), a.barrier));
        named.push(("aquifer.fluid_level_floodedness".into(), a.fluid_level_floodedness));
        named.push(("aquifer.fluid_level_spread".into(), a.fluid_level_spread));
        named.push(("aquifer.lava".into(), a.lava));
        named.push(("aquifer.exclusion".into(), a.exclusion));
        named.push(("aquifer.surface_level".into(), a.surface_level));
    }
    for (p, id) in extra.iter().zip(ids) {
        named.push((format!("fn.{p}"), id));
    }
    let only = std::env::var("ONLY").ok();
    let (mut ok, mut bad) = (0, Vec::new());
    for (name, id) in &named {
        if only.as_deref().is_some_and(|o| o != name) {
            continue;
        }
        let mut ctx = Ctx::uncached();
        let mut scalar = Vec::new();
        for &x in &XS {
            for &y in &YS {
                for &z in &ZS {
                    scalar.push(gen_.program.value(&mut ctx, *id, x, y, z));
                }
            }
        }
        let mut results = vec![("scalar".to_owned(), scalar)];
        for (k, s) in SHAPES.iter().enumerate() {
            let v = Volume::new([s[0], s[1], s[2]], [s[3], s[4], s[5]], [s[6], s[7], s[8]]);
            let mut buf = vec![0.0f32; v.len()];
            let mut ctx = Ctx::uncached();
            gen_.program.volume(&mut ctx, *id, &mut buf, &v);
            results.push((format!("vol{k}"), buf));
        }
        for (shape, vals) in results {
            let Some(exp) = want.get(&(name.clone(), shape.clone())) else { continue };
            let first: Vec<String> = vals.iter().take(6).map(|v| format!("{:x}", v.to_bits())).collect();
            let got = format!("{:x} {} {}", fnv(&vals), vals.len(), first.join(" "));
            if &got == exp {
                ok += 1;
            } else {
                bad.push(format!("{name} {shape}\n  want {exp}\n  got  {got}"));
            }
        }
    }
    assert!(ok > 0, "no comparisons made");
    assert!(bad.is_empty(), "{settings}: {} of {} mismatched\n{}", bad.len(), ok + bad.len(), bad.iter().take(12).cloned().collect::<Vec<_>>().join("\n"));
}

macro_rules! density_case {
    ($test:ident, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check($settings, $seed, include_str!(concat!("fixtures/release26_3/density-", $settings, "-", $seed, ".txt")));
        }
    };
}

density_case!(overworld_42, "overworld", 42);
density_case!(amplified_42, "amplified", 42);
density_case!(large_biomes_42, "large_biomes", 42);
density_case!(nether_42, "nether", 42);
density_case!(end_42, "end", 42);
density_case!(caves_42, "caves", 42);
density_case!(floating_islands_42, "floating_islands", 42);

/// Control: the comparison must fail for a different world seed, proving the
/// hashes discriminate and the pass above is not vacuous.
#[test]
#[should_panic(expected = "mismatched")]
fn control_wrong_seed_fails() {
    check("overworld", 43, include_str!("fixtures/release26_3/density-overworld-42.txt"));
}
