//! The structure-terrain term against the real server's own (`BeardifierOracle263`).
//! Scenarios come from the same text file the oracle reads.

use std::collections::BTreeMap;

mod common;
use common::scenarios;
use lodestone_worldgen_core::engine::release26_3::beardifier::{Beardifier, BoundingBox, Rigid, TerrainAdjustment};
use lodestone_worldgen_core::engine::release26_3::sampler::BeardifierSource;
use lodestone_worldgen_core::engine::release26_3::volume::Volume;

const XS: [i32; 13] = [-60, -31, -13, -5, 0, 3, 7, 12, 19, 26, 33, 41, 70];
const YS: [i32; 15] = [-64, 0, 21, 33, 45, 52, 58, 61, 63, 66, 70, 75, 91, 120, 300];
const ZS: [i32; 10] = [-50, -21, -8, 0, 5, 11, 17, 24, 35, 50];
const SHAPES: [[i32; 9]; 5] = [
    [16, 24, 16, 0, 56, 0, 1, 1, 1],
    [16, 24, 16, 16, 40, -16, 1, 1, 1],
    [8, 12, 8, -8, 20, -24, 4, 8, 4],
    [5, 7, 5, 3, 2, 3, 8, 16, 8],
    [16, 384, 16, -16, -64, 16, 1, 1, 1],
];

fn report(values: &[f32]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in values {
        for b in v.to_bits().to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let first: Vec<String> = values.iter().take(6).map(|v| format!("{:x}", v.to_bits())).collect();
    format!("{h:x} {} {}", values.len(), first.join(" "))
}

fn lines(name: &str, b: &Beardifier, seed: Option<&str>) -> Vec<String> {
    let _ = seed;
    let mut scalar = Vec::new();
    for &x in &XS {
        for &y in &YS {
            for &z in &ZS {
                scalar.push(b.value(x, y, z));
            }
        }
    }
    let mut out = vec![format!("beard {name} scalar {}", report(&scalar))];
    for (k, s) in SHAPES.iter().enumerate() {
        let v = Volume::new([s[0], s[1], s[2]], [s[3], s[4], s[5]], [s[6], s[7], s[8]]);
        let mut buf = vec![0.0f32; v.len()];
        b.fill_volume(&mut buf, &v);
        out.push(format!("beard {name} vol{k} {}", report(&buf)));
    }
    out
}

#[test]
fn matches_the_server() {
    let fixture = include_str!("fixtures/release26_3/beardifier.txt");
    let got: Vec<String> = scenarios().iter().flat_map(|(n, b)| lines(n, b, None)).collect();
    let mut want_lines: Vec<&str> = fixture.lines().collect();
    let mut got_sorted = got.clone();
    want_lines.sort_unstable();
    got_sorted.sort();
    assert_eq!(want_lines.len(), got_sorted.len());
    let bad: Vec<String> = want_lines.iter().zip(&got_sorted).filter(|(w, g)| w != g).map(|(w, g)| format!("want {w}\ngot  {g}")).collect();
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Control: moving one piece one block must change the hashes.
#[test]
fn control_shifted_piece_differs() {
    let fixture = include_str!("fixtures/release26_3/beardifier.txt");
    let b = Beardifier::new(
        vec![Rigid { bounds: BoundingBox { min: [5, 60, 4], max: [31, 75, 28] }, adjustment: TerrainAdjustment::BeardThin, ground_level_delta: 0 }],
        vec![],
    );
    let got = lines("thin", &b, None);
    assert!(got.iter().any(|l| !fixture.lines().any(|w| w == l)));
}
