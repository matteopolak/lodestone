//! Ore and scattered-ore placement against the real server (`DecorationOracle263`).
//!
//! Regenerate: `scripts/worldgen-oracle-26-3/run.sh DecorationOracle263 42 overworld
//! surface-biomes.txt ore,scattered_ore -64 384 0 0 3 -2 -5 4 > tests/fixtures/ore-overworld-42.txt`
//! (plus a few more chunk coordinates). Only the listed feature types run on both sides.

mod common;

use common::*;

fn check(seed: i64, fixture: &str, only: &[&str]) {
    let mut world = World::new("overworld", seed, SURFACE_BIOMES);
    let decorator = world.decorator();
    let mut checked = 0;
    let mut chunks: Vec<(i32, i32)> = Vec::new();
    for l in fixture.lines() {
        if let Some(rest) = l.strip_prefix("chunk ") {
            let f: Vec<&str> = rest.split(' ').collect();
            chunks.push((f[2].parse().unwrap(), f[3].parse().unwrap()));
        }
    }
    let mut sections: Vec<String> = Vec::new();
    for l in fixture.lines() {
        if l.starts_with("chunk ") {
            sections.push(String::new());
        }
        if let Some(s) = sections.last_mut() {
            s.push_str(l);
            s.push('\n');
        }
    }
    for ((cx, cz), want) in chunks.iter().zip(&sections) {
        let got = run_chunk(&mut world, &decorator, *cx, *cz, Some(only));
        compare(want, &got);
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn ores_overworld_42() {
    check(42, include_str!("fixtures/ore-overworld-42.txt"), &["ore", "scattered_ore"]);
}

/// Control: the same fixture under a neighbouring seed must be rejected, so a pass above
/// cannot come from a comparison that ignores the world.
#[test]
fn control_wrong_seed_fails() {
    let result = std::panic::catch_unwind(|| check(43, include_str!("fixtures/ore-overworld-42.txt"), &["ore", "scattered_ore"]));
    assert!(result.is_err(), "a different seed must not reproduce the oracle's chunks");
}

/// Control: the fixture really exercises placement (blobs written, buried and scattered ores).
#[test]
fn fixture_is_not_vacuous() {
    let fixture = include_str!("fixtures/ore-overworld-42.txt");
    let changed: Vec<u32> = fixture
        .lines()
        .filter(|l| l.starts_with("f "))
        .map(|l| l.split(' ').nth(7).unwrap().parse().unwrap())
        .collect();
    assert!(changed.iter().filter(|&&c| c > 0).count() > 40, "most chunks place several ore families");
    assert!(changed.iter().any(|&c| c > 500), "large blobs are present");
}

/// Decoration cost per chunk over the mixed-biome terrain (terrain excluded).
/// `cargo test -p lodestone-worldgen-feature-26-3 --release --test ore -- --ignored --nocapture`
#[test]
#[ignore = "timing"]
fn ore_decoration_cost() {
    let mut world = World::new("overworld", 42, SURFACE_BIOMES);
    let decorator = world.decorator();
    let coords = [(0, 0), (3, -2), (-5, 4), (-11, 7), (8, 8), (2, 9)];
    let mut levels: Vec<_> = coords.iter().map(|&(cx, cz)| (cx, cz, world.level(cx, cz), world.present(cx, cz))).collect();
    let t = std::time::Instant::now();
    let mut placed = 0usize;
    for (cx, cz, level, present) in &mut levels {
        decorator.decorate(level, *cx, *cz, present, Some(&["ore", "scattered_ore"]), |r| placed += r.changed.len());
    }
    let per = t.elapsed().as_secs_f64() * 1000.0 / levels.len() as f64;
    println!("ore decoration: {per:.2} ms/chunk ({placed} blocks over {} chunks)", levels.len());
}
