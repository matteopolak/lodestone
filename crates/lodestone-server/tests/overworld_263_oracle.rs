//! The 26.3 Overworld chunk source against the real 26.3 server's own generation.
//!
//! `scripts/worldgen-oracle-26-3/WorldOracle263.java` generates each target the way the source
//! does: the 5x5 chunks around it through the real fill, surface rules and carvers over the real
//! multi-noise biome source, then the real placed-feature decoration of the nine chunks around the
//! target in the production source order. The fixture holds, per target, one hash for every
//! 16-row section of the finished column and one for the whole column.
//!
//! Regenerate with `scripts/worldgen-oracle-26-3/run.sh WorldOracle263 <seed> <cx> <cz> ...` (see
//! `docs/worldgen-world-263.md`); the fixture header lines the container prints are dropped.

use lodestone_server::{ChunkSource, Overworld263ChunkSource};

const FIXTURE_42: &str = include_str!("fixtures/world-oracle-26-3/overworld-42.txt");

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
fn hashes(source: &Overworld263ChunkSource, cx: i32, cz: i32) -> (Vec<String>, String) {
    let column = source.column(cx, cz);
    let terrain = source.terrain();
    let keys = &terrain.env().blocks;
    let (mut full, mut sections) = (0xcbf2_9ce4_8422_2325u64, Vec::new());
    for section in 0..24 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for y in -64 + section * 16..-64 + section * 16 + 16 {
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
fn mismatches(seed: i64, fixture: &str) -> Vec<String> {
    let source = Overworld263ChunkSource::new(seed).expect("26.3 data compiles");
    let mut bad = Vec::new();
    for (cx, cz, want_sections, want_full) in parse(fixture) {
        let (got_sections, got_full) = hashes(&source, cx, cz);
        if got_full == want_full {
            continue;
        }
        let sections: Vec<String> = (0..24).filter(|&k| got_sections[k] != want_sections[k]).map(|k| k.to_string()).collect();
        bad.push(format!("chunk ({cx}, {cz}): sections [{}] differ", sections.join(", ")));
    }
    bad
}

#[test]
fn served_columns_match_the_real_26_3_server() {
    let bad = mismatches(42, FIXTURE_42);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Control: the same fixture must not be reproduced by another seed, so a pass above is a
/// content match and not a comparison that cannot fail.
#[test]
fn control_another_seed_does_not_reproduce_the_fixture() {
    let bad = mismatches(43, FIXTURE_42);
    assert_eq!(bad.len(), parse(FIXTURE_42).len(), "every target must differ under another seed");
}

/// Diagnostic: `LODESTONE_DUMP=cx,cz cargo test ... dump_target -- --ignored --nocapture` prints
/// every non-air block of that column in the oracle's `dump` format, for a line diff.
#[test]
#[ignore]
fn dump_target() {
    let Ok(spec) = std::env::var("LODESTONE_DUMP") else { return };
    let (cx, cz) = spec.split_once(',').expect("cx,cz");
    let (cx, cz): (i32, i32) = (cx.trim().parse().unwrap(), cz.trim().parse().unwrap());
    let source = Overworld263ChunkSource::new(42).expect("26.3 data compiles");
    let column = source.column(cx, cz);
    let terrain = source.terrain();
    let keys = &terrain.env().blocks;
    for qz in 0..4 {
        let shaped = terrain.shaped(cx, cz);
        let row: Vec<&str> = (0..4).map(|qx| terrain.biome_name(shaped.biomes.get(qx, 34 - 16, qz))).collect();
        println!("biome qz={qz} {row:?}");
    }
    for y in -64..320 {
        for z in 0..16 {
            for x in 0..16 {
                let key = keys.full_key(terrain.feature_state(column.block_state_id(x, y, z)));
                if !key.starts_with("minecraft:air") && !key.starts_with("minecraft:cave_air") && !key.starts_with("minecraft:void_air") {
                    println!("blk {x} {y} {z} {key}");
                }
            }
        }
    }
}
