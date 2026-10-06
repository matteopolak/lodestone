//! Structures in the 26.3 Overworld the game plays in, against the real 26.3 server.
//!
//! `scripts/worldgen-oracle-26-3/StructureOracle263.java` runs the real server's own structure
//! start creation (`chunks` and window modes) for seed 42 over chunks -60..=60 and over the first
//! three stronghold ring positions; `fixtures/world-oracle-26-3/structure-starts-42.txt` holds
//! its `start` lines (structure, adjusted bounding box, piece count) and every `ring` position.
//!
//! The production source (`overworld_263_chunk_source_of_type`) must reproduce each recorded start
//! at its chunk with the same structure, box and piece count, produce no start the oracle did not,
//! and place the structure's blocks into the columns it serves.
//!
//! One kind of difference is by design and is compared on its horizontal extent only: a
//! shipwreck, ocean ruin or swamp hut records a placeholder Y at start time that the real
//! server settles when the piece is written, whereas this engine settles it when the start is
//! made; a monument start carries its whole room tree here and a single piece there.

use std::collections::{BTreeMap, BTreeSet};

use lodestone_server::{ChunkSource, WorldType, overworld_263_chunk_source_of_type};

const FIXTURE: &str = include_str!("fixtures/world-oracle-26-3/structure-starts-42.txt");

/// Structures whose start records a Y or piece list the real server settles later.
const SETTLED_LATER: [&str; 6] = [
    "minecraft:shipwreck",
    "minecraft:shipwreck_beached",
    "minecraft:ocean_ruin_cold",
    "minecraft:ocean_ruin_warm",
    "minecraft:swamp_hut",
    "minecraft:monument",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Start {
    chunk: (i32, i32),
    structure: String,
    min: [i32; 3],
    max: [i32; 3],
    pieces: usize,
}

fn fixture_starts() -> Vec<Start> {
    FIXTURE
        .lines()
        .filter(|l| l.starts_with("start "))
        .map(|line| {
            let f: Vec<&str> = line.split(' ').collect();
            let n = |i: usize| f[i].parse::<i32>().unwrap();
            Start { chunk: (n(1), n(2)), structure: f[3].to_owned(), min: [n(4), n(5), n(6)], max: [n(7), n(8), n(9)], pieces: f[10].parse().unwrap() }
        })
        .collect()
}

fn fixture_rings() -> BTreeSet<(i32, i32)> {
    FIXTURE
        .lines()
        .filter(|l| l.starts_with("ring "))
        .map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            (f[1].parse().unwrap(), f[2].parse().unwrap())
        })
        .collect()
}

fn ours(source: &lodestone_server::Terrain263ChunkSource, cx: i32, cz: i32) -> Vec<Start> {
    source
        .terrain()
        .structure_starts(cx, cz)
        .iter()
        .map(|s| {
            let b = s.adjusted_bounding_box();
            Start { chunk: (cx, cz), structure: s.structure.clone(), min: b.min, max: b.max, pieces: s.pieces.len() }
        })
        .collect()
}

/// Where `got` and `want` disagree, one line per start.
fn differences(got: &[Start], want: &[Start]) -> Vec<String> {
    let mut bad = Vec::new();
    for w in want {
        let Some(g) = got.iter().find(|g| g.chunk == w.chunk && g.structure == w.structure) else {
            bad.push(format!("missing {} at {:?}", w.structure, w.chunk));
            continue;
        };
        let later = SETTLED_LATER.contains(&w.structure.as_str());
        let horizontal = g.min[0] == w.min[0] && g.min[2] == w.min[2] && g.max[0] == w.max[0] && g.max[2] == w.max[2];
        let exact = horizontal && g.min[1] == w.min[1] && g.max[1] == w.max[1] && g.pieces == w.pieces;
        if !(if later { horizontal } else { exact }) {
            bad.push(format!("{} at {:?}: got {:?}..{:?} x{}, oracle {:?}..{:?} x{}", w.structure, w.chunk, g.min, g.max, g.pieces, w.min, w.max, w.pieces));
        }
    }
    for g in got {
        if !want.iter().any(|w| w.chunk == g.chunk && w.structure == g.structure) {
            bad.push(format!("extra {} at {:?}", g.structure, g.chunk));
        }
    }
    bad
}

#[test]
fn starts_match_the_real_26_3_server() {
    let source = overworld_263_chunk_source_of_type(42, WorldType::Overworld);
    let want = fixture_starts();
    assert!(want.len() > 100, "the fixture holds the oracle's starts");
    let mut got = Vec::new();
    let mut seen = BTreeSet::new();
    for w in &want {
        if seen.insert(w.chunk) {
            got.extend(ours(&source, w.chunk.0, w.chunk.1));
        }
    }
    let bad = differences(&got, &want);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// No start appears where the oracle found none: every chunk of the oracle's window within 20
/// chunks of the origin is checked, not only the recorded ones.
#[test]
fn no_start_the_oracle_did_not_make() {
    let source = overworld_263_chunk_source_of_type(42, WorldType::Overworld);
    let mut got = Vec::new();
    for cz in -20..=20 {
        for cx in -20..=20 {
            got.extend(ours(&source, cx, cz));
        }
    }
    let want: Vec<Start> = fixture_starts().into_iter().filter(|s| s.chunk.0.abs() <= 20 && s.chunk.1.abs() <= 20).collect();
    assert!(!want.is_empty());
    let bad = differences(&got, &want);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn stronghold_rings_match_the_real_26_3_server() {
    let source = overworld_263_chunk_source_of_type(42, WorldType::Overworld);
    let got: BTreeSet<(i32, i32)> = source.terrain().ring_origins("minecraft:strongholds").into_iter().collect();
    assert_eq!(got, fixture_rings());
}

/// Control: another seed does not reproduce the starts, so a pass above is a content match.
#[test]
fn control_another_seed_does_not_reproduce_the_starts() {
    let source = overworld_263_chunk_source_of_type(43, WorldType::Overworld);
    let want = fixture_starts();
    let mut got = Vec::new();
    for w in want.iter().take(40) {
        got.extend(ours(&source, w.chunk.0, w.chunk.1));
    }
    let matching = want.iter().take(40).filter(|w| got.iter().any(|g| g.chunk == w.chunk && g.structure == w.structure && g.min == w.min)).count();
    assert!(matching < 10, "another seed matched {matching} of 40 recorded starts");
}

/// Counts of each block id in the column.
fn census(source: &lodestone_server::Terrain263ChunkSource, cx: i32, cz: i32) -> BTreeMap<String, usize> {
    let column = source.column(cx, cz);
    let mut counts = BTreeMap::new();
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                let state = column.block_state_id(x, y, z);
                *counts.entry(state.block().name().to_owned()).or_insert(0) += 1;
            }
        }
    }
    counts
}

/// The plains village recorded at chunk (41, -19) is built into the columns the source serves,
/// and a source without structures serves none of it.
#[test]
fn served_columns_contain_the_village() {
    let with = overworld_263_chunk_source_of_type(42, WorldType::Overworld);
    let without = lodestone_server::Terrain263ChunkSource::new(42).expect("26.3 data compiles");
    // The recorded village covers x 571..720, z -384..-228; sample a column on its main street.
    let (cx, cz) = (40, -20);
    let built = census(&with, cx, cz);
    let bare = census(&without, cx, cz);
    let planks = |m: &BTreeMap<String, usize>| m.get("minecraft:oak_planks").copied().unwrap_or(0) + m.get("minecraft:cobblestone").copied().unwrap_or(0) + m.get("minecraft:dirt_path").copied().unwrap_or(0);
    assert!(planks(&built) > 50, "village blocks in the served column: {built:?}");
    assert!(planks(&built) > planks(&bare) + 50, "control without structures: {bare:?}");
}
