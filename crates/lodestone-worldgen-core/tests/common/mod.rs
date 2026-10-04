//! Scenario file shared with the JVM oracles (`scripts/worldgen-oracle-26-3/beard-scenarios.txt`).
#![allow(dead_code)]

use std::collections::BTreeMap;

use lodestone_worldgen_core::engine::release26_3::beardifier::{Beardifier, BoundingBox, Junction, Rigid, TerrainAdjustment};

pub const SCENARIOS: &str = include_str!("../../../../scripts/worldgen-oracle-26-3/beard-scenarios.txt");

pub fn scenarios() -> BTreeMap<String, Beardifier> {
    let mut out = BTreeMap::new();
    let (mut name, mut rigids, mut junctions) = (String::new(), Vec::new(), Vec::new());
    for line in SCENARIOS.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.first().copied() {
            None | Some("#") => {}
            Some(c) if c.starts_with('#') => {}
            Some("scenario") => {
                name = f[1].to_owned();
                rigids = Vec::new();
                junctions = Vec::new();
            }
            Some("rigid") => {
                let n: Vec<i32> = f[1..7].iter().map(|v| v.parse().unwrap()).collect();
                let adjustment = match f[7] {
                    "none" => TerrainAdjustment::None,
                    "bury" => TerrainAdjustment::Bury,
                    "beard_thin" => TerrainAdjustment::BeardThin,
                    "beard_box" => TerrainAdjustment::BeardBox,
                    "encapsulate" => TerrainAdjustment::Encapsulate,
                    other => panic!("unknown adjustment {other}"),
                };
                rigids.push(Rigid {
                    bounds: BoundingBox { min: [n[0], n[1], n[2]], max: [n[3], n[4], n[5]] },
                    adjustment,
                    ground_level_delta: f[8].parse().unwrap(),
                });
            }
            Some("junction") => junctions.push(Junction {
                source_x: f[1].parse().unwrap(),
                source_ground_y: f[2].parse().unwrap(),
                source_z: f[3].parse().unwrap(),
            }),
            Some("end") => {
                out.insert(name.clone(), Beardifier::new(std::mem::take(&mut rigids), std::mem::take(&mut junctions)));
            }
            Some(other) => panic!("unknown scenario directive {other}"),
        }
    }
    out
}


pub const SURFACE_STATES: &str = include_str!("../fixtures/release26_3/surface-states.txt");

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

/// The oracle's per-chunk report, `<settings> <seed> <cx> <cz> blocks <hash> counts <...> heights <hash> post <hash> <n>`:
/// every block (z, x, then y ascending) by its full state key, the world-surface
/// heightmap, and the post-processing lists.
pub fn chunk_report(
    g: &lodestone_worldgen_core::engine::release26_3::settings::TerrainGenerator,
    chunk: &lodestone_worldgen_core::engine::release26_3::surface::SurfaceChunk,
    settings: &str,
    seed: i64,
    cx: i32,
    cz: i32,
) -> String {
    use std::collections::{BTreeMap, HashMap};
    let full: HashMap<&str, &str> = SURFACE_STATES
        .lines()
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f[1], f[2])
        })
        .collect();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for z in 0..16 {
        for x in 0..16 {
            for y in chunk.min_y..chunk.min_y + chunk.height {
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
    format!("{settings} {seed} {cx} {cz} blocks {h:x} counts {} heights {hh:x} post {ph:x} {total}", counts.join(";"))
}
