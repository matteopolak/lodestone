//! Caves, oceans and structure-like features (lichen, dripstone, geodes, fossils, icebergs, pale
//! garden, ...) against the real server (`DecorationOracle263`), over a layout of those biomes. Every placed feature without a
//! known gap runs on both sides; regenerate the fixture whenever a port closes a gap:
//! run the ignored `print_oracle_args` test and feed its output to the oracle with
//! `special-biomes.txt` as the layout.

mod common;

use common::*;

const FIXTURE: &str = include_str!("fixtures/specials-overworld-42.txt");

fn world() -> World {
    World::new("overworld", 42, SPECIAL_BIOMES)
}

fn only() -> Vec<String> {
    all_ported(&world().decorator())
}

#[test]
#[ignore = "prints the oracle arguments"]
fn print_oracle_args() {
    println!("ARGS {}", only().join(","));
}

#[test]
#[ignore = "prints the remaining gaps"]
fn print_gaps() {
    let mut by_gap: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (name, gaps) in world().decorator().gaps(env()) {
        for g in gaps {
            by_gap.entry(g).or_default().push(name.clone());
        }
    }
    for (gap, names) in by_gap {
        println!("{gap}: {} ({})", names.len(), names.iter().take(8).cloned().collect::<Vec<_>>().join(", "));
    }
}

#[test]
fn specials_overworld_42() {
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    check_in(SPECIAL_BIOMES, 42, FIXTURE, &refs);
}

const LUSH_FIXTURE: &str = include_str!("fixtures/lush-only-overworld-42.txt");

/// A layout of lush caves alone, so the rare azalea root systems and ceiling plants place.
#[test]
fn lush_only_overworld_42() {
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    check_in(LUSH_BIOMES, 42, LUSH_FIXTURE, &refs);
    for name in ["minecraft:rooted_azalea_tree", "minecraft:spore_blossom", "minecraft:lush_caves_vegetation"] {
        let hit = LUSH_FIXTURE.lines().filter(|l| l.starts_with("f ") && l.contains(name)).any(|l| l.split(' ').nth(7) != Some("0"));
        assert!(hit, "{name} never changes a block in the lush fixture");
    }
}

const ICE_FIXTURE: &str = include_str!("fixtures/ice-only-overworld-42.txt");

/// Frozen oceans alone, so the rare icebergs and blue ice patches place.
#[test]
fn ice_only_overworld_42() {
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    check_in(ICE_BIOMES, 42, ICE_FIXTURE, &refs);
    for name in ["minecraft:iceberg_packed", "minecraft:iceberg_blue", "minecraft:blue_ice"] {
        let hit = ICE_FIXTURE.lines().filter(|l| l.starts_with("f ") && l.contains(name)).any(|l| l.split(' ').nth(7) != Some("0"));
        assert!(hit, "{name} never changes a block in the ice fixture");
    }
}

const WARM_FIXTURE: &str = include_str!("fixtures/warm-ocean-only-overworld-42.txt");

/// Warm ocean alone, so the coral trees, claws and mushrooms place on most chunks.
#[test]
fn warm_ocean_only_overworld_42() {
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    check_in(WARM_BIOMES, 42, WARM_FIXTURE, &refs);
    let changed: u32 = WARM_FIXTURE
        .lines()
        .filter(|l| l.starts_with("f ") && l.contains(" minecraft:warm_ocean_vegetation "))
        .map(|l| l.split(' ').nth(7).unwrap().parse::<u32>().unwrap())
        .sum();
    assert!(changed > 500, "warm_ocean_vegetation changed only {changed} cells in the warm ocean fixture");
}

const ROOMS_FIXTURE: &str = include_str!("fixtures/rooms-overworld-42.txt");

/// Dungeons are rare in noise-only terrain; two chunks found by a 576-chunk sweep hold deep ones,
/// and a third chunk with none is a control that a miss is also a match.
#[test]
fn monster_rooms_overworld_42() {
    check_in(PLAINS_BIOMES, 42, ROOMS_FIXTURE, &["monster_room"]);
    let changed: u32 = ROOMS_FIXTURE
        .lines()
        .filter(|l| l.starts_with("f ") && l.contains(" minecraft:monster_room_deep "))
        .map(|l| l.split(' ').nth(7).unwrap().parse::<u32>().unwrap())
        .sum();
    assert!(changed > 400, "the room fixture changed only {changed} cells");
    let wrong = std::panic::catch_unwind(|| check_in(PLAINS_BIOMES, 43, ROOMS_FIXTURE, &["monster_room"]));
    assert!(wrong.is_err(), "a different seed must not reproduce the rooms");
}

const GEODE_FIXTURE: &str = include_str!("fixtures/geodes-overworld-42.txt");

/// Four chunks of a plains sweep that hold amethyst geodes (about one chunk in thirty does).
#[test]
fn geodes_overworld_42() {
    check_in(PLAINS_BIOMES, 42, GEODE_FIXTURE, &["geode"]);
    let changed: u32 = GEODE_FIXTURE
        .lines()
        .filter(|l| l.starts_with("f ") && l.contains(" minecraft:amethyst_geode "))
        .map(|l| l.split(' ').nth(7).unwrap().parse::<u32>().unwrap())
        .sum();
    assert!(changed > 5000, "the geode fixture changed only {changed} cells");
    let wrong = std::panic::catch_unwind(|| check_in(PLAINS_BIOMES, 43, GEODE_FIXTURE, &["geode"]));
    assert!(wrong.is_err(), "a different seed must not reproduce the geodes");
}

/// Control: the same fixture under a neighbouring seed must be rejected.
#[test]
fn control_wrong_seed_fails() {
    let only = only();
    let result = std::panic::catch_unwind(|| {
        let refs: Vec<&str> = only.iter().map(String::as_str).collect();
        check_in(SPECIAL_BIOMES, 43, FIXTURE, &refs)
    });
    assert!(result.is_err(), "a different seed must not reproduce the oracle's chunks");
}

/// Control: the listed features really change blocks in the fixture.
#[test]
fn fixture_is_not_vacuous() {
    let mut changed: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for l in FIXTURE.lines().filter(|l| l.starts_with("f ")) {
        let f: Vec<&str> = l.split(' ').collect();
        *changed.entry(f[3]).or_default() += f[7].parse::<u32>().unwrap();
    }
    for name in NON_VACUOUS {
        assert!(changed.get(*name).copied().unwrap_or(0) > 0, "{name} never changes a block in the fixture");
    }
}

/// Features that must change blocks somewhere in the fixture.
const NON_VACUOUS: &[&str] = &[
    "minecraft:glow_lichen",
    "minecraft:sculk_vein",
    "minecraft:ice_spike",
    "minecraft:large_dripstone",
    "minecraft:pointed_dripstone",
    "minecraft:dripstone_cluster",
    "minecraft:sulfur_spike",
    "minecraft:sulfur_spike_cluster",
    "minecraft:sulfur_pool",
    "minecraft:lush_caves_clay",
    "minecraft:lush_caves_vegetation",
    "minecraft:lush_caves_ceiling_vegetation",
    "minecraft:pale_moss_patch",
];

/// Prints the cells one placed feature changes in one chunk, in the oracle's `d` format
/// (`LODESTONE_DUMP=<placed name>,<cx>,<cz>`), for diffing against the oracle's `+dump:name`.
#[test]
#[ignore = "debugging aid"]
fn dump_feature() {
    let spec = std::env::var("LODESTONE_DUMP").expect("LODESTONE_DUMP=name,cx,cz");
    let f: Vec<&str> = spec.split(',').collect();
    let (name, cx, cz): (&str, i32, i32) = (f[0], f[1].parse().unwrap(), f[2].parse().unwrap());
    let mut world = world();
    let decorator = world.decorator();
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    let mut level = world.level(cx, cz);
    let present = world.present(cx, cz);
    let blocks = &env().blocks;
    decorator.decorate(&mut level, cx, cz, &present, Some(&refs), |r| {
        if decorator.features.placed[r.placed].name.ends_with(name) {
            for (x, y, z, s) in &r.changed {
                println!("d {x} {y} {z} {}", blocks.full_key(*s));
            }
            println!("f draws {} changed {}", r.draws, r.changed.len());
        }
    });
}
