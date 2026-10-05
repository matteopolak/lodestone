//! Ore and scattered-ore placement against the real server (`DecorationOracle263`).
//!
//! Regenerate: `scripts/worldgen-oracle-26-3/run.sh DecorationOracle263 42 overworld
//! surface-biomes.txt ore,scattered_ore -64 384 0 0 3 -2 -5 4 > tests/fixtures/ore-overworld-42.txt`
//! (plus a few more chunk coordinates). Only the listed feature types run on both sides.

mod common;

use common::*;

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
