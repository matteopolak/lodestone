//! Surface building (material rules, heightmap, post-processing) against the real
//! server's fill plus surface step (`SurfaceOracle263`), over a synthetic biome source
//! defined by `scripts/worldgen-oracle-26-3/surface-biomes.txt` and the hash below.

use std::collections::{BTreeMap, HashMap};

use lodestone_worldgen_core::engine::release26_3::biome::{BiomeId, obfuscate_seed};
use lodestone_worldgen_core::engine::release26_3::sampler::Ctx;
use lodestone_worldgen_core::engine::release26_3::settings::{ResourceSet, TerrainGenerator};
use lodestone_worldgen_data_26_3 as data;

const BIOMES: &str = include_str!("../../../scripts/worldgen-oracle-26-3/surface-biomes.txt");
const STATES: &str = include_str!("fixtures/release26_3/surface-states.txt");

fn fnv_bytes(h: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *h ^= u64::from(*b);
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    fnv_bytes(&mut h, s.as_bytes());
    h
}

fn pick(qx: i32, qy: i32, qz: i32, n: usize) -> usize {
    let mut h = (qx >> 1).wrapping_mul(73_856_093) ^ (qy >> 3).wrapping_mul(19_349_663) ^ (qz >> 1).wrapping_mul(83_492_791);
    h ^= ((h as u32) >> 13) as i32;
    h = h.wrapping_mul(0x5bd1_e995);
    h ^= ((h as u32) >> 15) as i32;
    h.rem_euclid(n as i32) as usize
}

fn line(settings: &str, seed: i64, cx: i32, cz: i32, g: &TerrainGenerator, full: &HashMap<&str, &str>) -> String {
    let names: Vec<&str> = BIOMES.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let ids: Vec<BiomeId> = names.iter().map(|n| g.biomes.id(n).expect("biome known")).collect();
    let (min_y, height) = match settings {
        "overworld" => (-64, 384),
        "nether" => (0, 128),
        _ => (0, 256),
    };
    let mut ctx = Ctx::new(&g.program);
    let fill = g.fill_chunk(cx, cz, &mut ctx);
    let mut source = |qx: i32, qy: i32, qz: i32| ids[pick(qx, qy, qz, ids.len())];
    let chunk = g.build_surface(&fill, cx, cz, min_y, height, &mut source, &mut ctx);

    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for z in 0..16 {
        for x in 0..16 {
            for y in min_y..min_y + height {
                let key = full.get(g.state_key(chunk.get(x, y, z).unwrap())).copied().expect("state in statemap");
                h = (h ^ fnv(key)).wrapping_mul(0x0000_0100_0000_01b3);
                *counts.entry(key).or_default() += 1;
            }
        }
    }
    let mut hh = 0xcbf2_9ce4_8422_2325u64;
    for z in 0..16 {
        for x in 0..16 {
            fnv_bytes(&mut hh, &chunk.surface_height(x, z).to_le_bytes());
        }
    }
    let mut ph = 0xcbf2_9ce4_8422_2325u64;
    let mut total = 0;
    for (i, list) in chunk.post_processing.iter().enumerate() {
        if list.is_empty() {
            continue;
        }
        fnv_bytes(&mut ph, &(i as i32).to_le_bytes());
        for v in list {
            fnv_bytes(&mut ph, &v.to_le_bytes());
            total += 1;
        }
    }
    let counts: Vec<String> = counts.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!(
        "surface {settings} {seed} {cx} {cz} blocks {h:x} counts {} heights {hh:x} post {ph:x} {total}",
        counts.join(";")
    )
}

fn generator(settings: &str, seed: i64) -> TerrainGenerator {
    let res = ResourceSet::from_tables(data::DENSITY_FUNCTION, data::NOISE, data::NOISE_SETTINGS).with_surface_data(
        data::MATERIAL_RULE,
        data::MATERIAL_CONDITION,
        data::BIOME,
    );
    TerrainGenerator::load(&res, settings, seed).expect("loads")
}

fn check(settings: &str, seed: i64, fixture: &str, generator_seed: i64) {
    let full: HashMap<&str, &str> = STATES
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f[1], f[2])
        })
        .collect();
    let g = generator(settings, generator_seed);
    let mut bad = Vec::new();
    let mut n = 0;
    for want in fixture.lines() {
        let f: Vec<&str> = want.split(' ').collect();
        if f[0] == "zoom" {
            // The oracle's own obfuscated seed for the world seed it was run with.
            let want: i64 = f[2].parse().unwrap();
            assert_eq!(obfuscate_seed(f[1].parse().unwrap()), want);
            if generator_seed == seed {
                assert_eq!(obfuscate_seed(generator_seed), want);
            }
            continue;
        }
        n += 1;
        let got = line(settings, seed, f[3].parse().unwrap(), f[4].parse().unwrap(), &g, &full);
        if got != want {
            bad.push(format!("want {want}\ngot  {got}"));
        }
    }
    assert!(n > 0);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

macro_rules! surface_case {
    ($test:ident, $settings:literal, $seed:literal) => {
        #[test]
        fn $test() {
            check($settings, $seed, include_str!(concat!("fixtures/release26_3/surface-", $settings, "-", $seed, ".txt")), $seed);
        }
    };
}

surface_case!(overworld_42, "overworld", 42);
surface_case!(overworld_7_sweep, "overworld", 7);
surface_case!(overworld_negative_seed, "overworld", -987654321);
surface_case!(nether_42, "nether", 42);
surface_case!(end_42, "end", 42);

/// Control: a different world seed must not reproduce the fixture.
#[test]
#[should_panic(expected = "want")]
fn control_wrong_seed_fails() {
    check("overworld", 42, include_str!("fixtures/release26_3/surface-overworld-42.txt"), 43);
}

/// The zoom seed is checked against the oracle's value for each fixture seed; a
/// neighbouring seed must not produce it (control for the digest, not just the
/// comparison).
#[test]
fn zoom_seed_matches_oracle_and_differs_for_other_seeds() {
    for (fixture, seed) in [
        (include_str!("fixtures/release26_3/surface-overworld-42.txt"), 42),
        (include_str!("fixtures/release26_3/surface-overworld--987654321.txt"), -987_654_321),
        (include_str!("fixtures/release26_3/surface-overworld-7.txt"), 7),
    ] {
        let want: i64 = fixture.lines().next().unwrap().split(' ').nth(2).unwrap().parse().unwrap();
        assert_eq!(obfuscate_seed(seed), want);
        assert_ne!(obfuscate_seed(seed + 1), want);
    }
}
