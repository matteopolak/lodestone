//! The 26.3 Nether and End chunk sources against the real 26.3 server's own generation.
//!
//! `scripts/worldgen-oracle-26-3/WorldOracle263.java`, given a leading `nether` or `end`,
//! generates each target the way the source does (the 5x5 chunks around it through the real
//! fill, surface rules and carvers over the dimension's real biome source, then the real
//! decoration of the nine chunks around the target in the production source order) over the
//! dimension's 0..256 build range, with no sky light in the Nether. The fixtures hold, per
//! target, one hash for every 16-row section of the finished column and one for the whole column.
//! Structures are not part of either side.
//!
//! Regenerate with `scripts/worldgen-oracle-26-3/run.sh WorldOracle263 <nether|end> <seed> <cx>
//! <cz> ...`, keeping only the `target`, `sec` and `full` lines.

use lodestone_server::{ChunkSource, Terrain263ChunkSource};

const NETHER_42: &str = include_str!("fixtures/world-oracle-26-3/nether-42.txt");
const END_42: &str = include_str!("fixtures/world-oracle-26-3/end-42.txt");

/// `(cx, cz, section hashes, full hash)` per target in the fixture.
fn parse(fixture: &str) -> Vec<(i32, i32, Vec<String>, String)> {
    let mut out = Vec::new();
    for line in fixture.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        match f[0] {
            "target" => out.push((f[2].parse().unwrap(), f[3].parse().unwrap(), Vec::new(), String::new())),
            "sec" => out.last_mut().unwrap().2.push(f[2].to_owned()),
            "full" => out.last_mut().unwrap().3 = f[1].to_owned(),
            other => panic!("unknown fixture line {other}"),
        }
    }
    out
}

/// The oracle's section and full-column hashes of what the source serves for `(cx, cz)`.
fn hashes(source: &Terrain263ChunkSource, cx: i32, cz: i32) -> (Vec<String>, String) {
    let column = source.column(cx, cz);
    let terrain = source.terrain();
    let keys = &terrain.env().blocks;
    let (mut full, mut sections) = (0xcbf2_9ce4_8422_2325u64, Vec::new());
    for section in 0..source.height() / 16 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for y in source.min_y() + section * 16..source.min_y() + section * 16 + 16 {
            for z in 0..16 {
                for x in 0..16 {
                    let key = keys.key_hash(terrain.feature_state(column.block_state_id(x, y, z)));
                    h = (h ^ key).wrapping_mul(0x0000_0100_0000_01b3);
                    full = (full ^ key).wrapping_mul(0x0000_0100_0000_01b3);
                }
            }
        }
        sections.push(format!("{h:x}"));
    }
    (sections, format!("{full:x}"))
}

/// Where a column first disagrees with the oracle, as `(cx, cz) section k` lines.
fn mismatches(settings: &str, seed: i64, fixture: &str) -> Vec<String> {
    let source = Terrain263ChunkSource::with_settings(seed, settings).expect("26.3 data compiles");
    let mut bad = Vec::new();
    for (cx, cz, want_sections, want_full) in parse(fixture) {
        let (got_sections, got_full) = hashes(&source, cx, cz);
        if got_full == want_full {
            continue;
        }
        let sections: Vec<String> = (0..want_sections.len()).filter(|&k| got_sections[k] != want_sections[k]).map(|k| k.to_string()).collect();
        bad.push(format!("{settings} chunk ({cx}, {cz}): sections [{}] differ", sections.join(", ")));
    }
    bad
}

#[test]
fn served_nether_columns_match_the_real_26_3_server() {
    let bad = mismatches("nether", 42, NETHER_42);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn served_end_columns_match_the_real_26_3_server() {
    let bad = mismatches("end", 42, END_42);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Control: neither fixture is reproduced by another seed, so a pass above is a content match.
/// The End's central island is the same for every seed except its pillars, so its targets that
/// hold no pillar may match; at least one must differ.
#[test]
fn control_another_seed_does_not_reproduce_the_fixtures() {
    assert_eq!(mismatches("nether", 43, NETHER_42).len(), parse(NETHER_42).len(), "every Nether target must differ");
    assert!(!mismatches("end", 43, END_42).is_empty(), "some End target must differ");
}

/// The source's dimension shape: a 0..256 column, bedrock floor in the Nether.
#[test]
fn nether_columns_span_the_dimension() {
    let source = Terrain263ChunkSource::with_settings(42, "nether").expect("26.3 data compiles");
    assert_eq!((source.min_y(), source.height()), (0, 256));
    let column = source.column(0, 0);
    assert_eq!(column.block_state_id(0, 0, 0).name(), "minecraft:bedrock");
    assert!(column.biome_state_at(0, 200, 0).starts_with("minecraft:"), "biomes cover the rows above the noise range");
}
