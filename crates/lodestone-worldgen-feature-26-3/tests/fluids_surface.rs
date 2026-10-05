//! Lakes, springs, disks and the top-layer freeze against the real server
//! (`DecorationOracle263`).
//!
//! Regenerate (then delete the log lines the JVM prints before the first `chunk` line):
//! `scripts/worldgen-oracle-26-3/run.sh DecorationOracle263 42 overworld surface-biomes.txt
//! lake,spring_feature,disk,freeze_top_layer -64 384 -6 -8 -6 1 7 -5 0 0 1 0 2 0 2 1 2 2 3 3 1 1
//! 2 5 3 5 0 4 5 0 > tests/fixtures/fluids-surface-overworld-42.txt`.
//! Only the listed feature types run on both sides; the three lake chunks are the only
//! underground lakes among 256 chunks around the origin.

mod common;

use common::*;

const FIXTURE: &str = include_str!("fixtures/fluids-surface-overworld-42.txt");
const TYPES: &[&str] = &["lake", "spring_feature", "disk", "freeze_top_layer"];

#[test]
fn fluids_and_surface_overworld_42() {
    check(42, FIXTURE, TYPES);
}

/// Control: the same fixture under a neighbouring seed must be rejected.
#[test]
fn control_wrong_seed_fails() {
    let result = std::panic::catch_unwind(|| check(43, FIXTURE, TYPES));
    assert!(result.is_err(), "a different seed must not reproduce the oracle's chunks");
}

/// Control: the fixture really exercises every stage (a stage that placed nothing could pass
/// by matching an empty oracle).
#[test]
fn fixture_is_not_vacuous() {
    let mut changed: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for l in FIXTURE.lines().filter(|l| l.starts_with("f ")) {
        let f: Vec<&str> = l.split(' ').collect();
        *changed.entry(f[3]).or_default() += f[7].parse::<u32>().unwrap();
    }
    for name in [
        "minecraft:lake_lava_underground",
        "minecraft:spring_water",
        "minecraft:spring_lava",
        "minecraft:disk_sand",
        "minecraft:disk_clay",
        "minecraft:disk_gravel",
        "minecraft:disk_grass",
        "minecraft:freeze_top_layer",
    ] {
        assert!(changed.get(name).copied().unwrap_or(0) > 0, "{name} never changes a block in the fixture");
    }
}
