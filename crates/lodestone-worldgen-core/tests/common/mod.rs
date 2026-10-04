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

